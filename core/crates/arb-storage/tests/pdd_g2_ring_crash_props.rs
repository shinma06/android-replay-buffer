//! Layer-3 PDD (G2): real `RingStore` + fake disk + fake host clock that resets on reboot,
//! crash injection around every pin/unpin boundary, REQ-04 pinned-byte preservation and
//! legacy schema-4 fixture immutability.

#![allow(
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::struct_excessive_bools,
    clippy::fn_params_excessive_bools,
    clippy::trivially_copy_pass_by_ref,
    clippy::print_stdout,
    clippy::collapsible_if,
    clippy::similar_names,
    clippy::format_collect,
    clippy::as_conversions,
    clippy::clone_on_ref_ptr,
    reason = "PDD model/property code: long linearized models and generators read best unsplit"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arb_core::state::RingBudgetPolicy;
use arb_os::error::OsError;
use arb_os::fs::DiskSpaceInfo;
use arb_storage::ring::{CaptureChunkInfo, DiskSpaceProvider, RingChunkKind, RingStore};
use proptest::prelude::*;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
struct FakeDisk(Arc<AtomicU64>);

impl DiskSpaceProvider for FakeDisk {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        Ok(DiskSpaceInfo {
            available_bytes: self.0.load(Ordering::SeqCst),
            total_bytes: 1 << 40,
        })
    }
}

#[derive(Default)]
struct Stats {
    writes: u64,
    pins: u64,
}

static STATS_W: AtomicU64 = AtomicU64::new(0);
static STATS_P: AtomicU64 = AtomicU64::new(0);

const FLOOR: u64 = 1_000_000;

fn policy() -> RingBudgetPolicy {
    RingBudgetPolicy {
        max_ring_bytes: 4096,
        max_ring_duration_ns: 60_000_000_000,
        min_free_disk_bytes: FLOOR,
    }
}

#[derive(Debug, Clone)]
enum Op {
    Write(u8, usize),
    Pin(usize),
    Unpin(usize),
    Disk(u8),
    CrashReopen,
    HostReboot,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (any::<u8>(), 1usize..900).prop_map(|(b, n)| Op::Write(b, n)),
        2 => (1usize..4).prop_map(Op::Pin),
        1 => (0usize..4).prop_map(Op::Unpin),
        1 => (0u8..3).prop_map(Op::Disk),
        1 => Just(Op::CrashReopen),
        1 => Just(Op::HostReboot),
    ]
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn tree_digests(dir: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("read fixture dir") {
            let p = e.expect("entry").path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), sha(&std::fs::read(&p).expect("read fixture")));
            }
        }
    }
    out
}

fn legacy_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../fixtures/legacy-schema4/replay-trim-parity")
        .join("replay-20240101-000000-manual-feedface")
}

fn info(boot: i64, cont: i64) -> CaptureChunkInfo {
    CaptureChunkInfo {
        host_boot_ns: boot,
        host_live: true,
        device_serial: "emulator-5554".into(),
        ingest_range: (cont, cont + 1),
        has_video: true,
        has_keyframe: true,
        has_video_config: true,
        has_audio: false,
        has_audio_config: false,
        has_anchor: false,
        heartbeat: None,
        video_size: (16, 16),
        video_config: None,
        audio_config: None,
    }
}

/// Mirrors `arbd::capture::restore_capture`: chunks of an earlier host boot are excluded and
/// their unpinned files purged so stale, larger `created_cont_ns` values cannot starve the
/// new boot.
fn restore_like_arbd(ring: &mut RingStore<FakeDisk>, boot: i64) {
    let stale: Vec<String> = ring
        .chunks()
        .into_iter()
        .filter(|c| c.capture.as_ref().is_none_or(|i| i.host_boot_ns != boot))
        .map(|c| c.chunk_id)
        .collect();
    let _ = ring.purge_unpinned_chunks(&stale);
}

fn run(ops: &[Op]) -> Result<(), TestCaseError> {
    let tmp = tempfile::tempdir().map_err(|e| TestCaseError::fail(e.to_string()))?;
    let data = tmp.path().join("data");
    let legacy = data.join("bundles").join("legacy-feedface");
    std::fs::create_dir_all(&legacy).map_err(|e| TestCaseError::fail(e.to_string()))?;
    for (src, _) in tree_digests(&legacy_fixture()) {
        let name = src.file_name().ok_or_else(|| TestCaseError::fail("name"))?;
        std::fs::copy(&src, legacy.join(name)).map_err(|e| TestCaseError::fail(e.to_string()))?;
    }
    let legacy_before = tree_digests(&legacy);
    let disk = Arc::new(AtomicU64::new(1 << 32));
    let open = || RingStore::open_with_disk_provider(&data, policy(), FakeDisk(disk.clone()));
    let mut ring = open().map_err(|e| TestCaseError::fail(e.to_string()))?;

    let mut boot = 1_000_000_000_000_i64;
    let mut clock = 5_000_000_000_i64;
    let mut last_created: Option<i64> = None;
    let mut seq = 0u32;
    let mut job_seq = 0u32;
    // chunk id -> payload digest for chunks protected by a live pin journal.
    let mut pinned: BTreeMap<String, String> = BTreeMap::new();
    let mut payloads: BTreeMap<String, String> = BTreeMap::new();
    let mut jobs: Vec<(String, Vec<String>)> = Vec::new();
    let mut wrote_this_boot = false;
    let mut stats = Stats::default();

    for op in ops {
        match *op {
            Op::Write(byte, len) => {
                clock += 1_000_000_000;
                let created = last_created.map_or(clock, |p| clock.max(p + 1));
                seq += 1;
                let id = format!("chunk-{seq}");
                let payload = vec![byte; len];
                let res = ring.write_capture_chunk(
                    id.clone(),
                    RingChunkKind::Video,
                    created,
                    1_000_000_000,
                    &payload,
                    Some(info(boot, created)),
                );
                let free = disk.load(Ordering::SeqCst);
                if free.saturating_sub(len as u64) < FLOOR {
                    prop_assert!(res.is_err(), "disk floor must stop writes");
                }
                if let Ok((desc, evicted)) = res {
                    stats.writes += 1;
                    if let Some(d) = desc.sha256.as_deref() {
                        let want = sha(&payload);
                        prop_assert_eq!(d, want.as_str(), "descriptor digest matches payload");
                    }
                    for e in &evicted {
                        prop_assert!(
                            !pinned.contains_key(e),
                            "REQ-04: pinned chunk {} evicted",
                            e
                        );
                        payloads.remove(e);
                    }
                    payloads.insert(id, sha(&payload));
                    last_created = Some(created);
                    wrote_this_boot = true;
                }
            }
            Op::Pin(k) => {
                let ids: Vec<String> = ring
                    .chunks()
                    .into_iter()
                    .rev()
                    .take(k)
                    .map(|c| c.chunk_id)
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                job_seq += 1;
                let job = format!("job-{job_seq}");
                ring.pin_chunks(
                    job.clone(),
                    tmp.path().join("out.arb"),
                    ids.clone(),
                    "2026-01-01T00:00:00Z",
                )
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
                for id in &ids {
                    let d = payloads
                        .get(id)
                        .cloned()
                        .ok_or_else(|| TestCaseError::fail("pin unknown"))?;
                    pinned.insert(id.clone(), d);
                }
                stats.pins += 1;
                jobs.push((job, ids));
            }
            Op::Unpin(i) => {
                if i < jobs.len() {
                    let (job, _) = jobs.remove(i);
                    prop_assert!(
                        ring.unpin_job(&job)
                            .map_err(|e| TestCaseError::fail(e.to_string()))?
                    );
                    let still: std::collections::BTreeSet<String> = jobs
                        .iter()
                        .flat_map(|(_, ids)| ids.iter().cloned())
                        .collect();
                    pinned.retain(|id, _| still.contains(id));
                }
            }
            Op::Disk(level) => {
                disk.store(
                    [1 << 32, FLOOR + 512, FLOOR / 2][usize::from(level)],
                    Ordering::SeqCst,
                );
            }
            Op::CrashReopen => {
                drop(ring);
                ring = open().map_err(|e| TestCaseError::fail(e.to_string()))?;
                restore_like_arbd(&mut ring, boot);
            }
            Op::HostReboot => {
                drop(ring);
                boot += 3_600_000_000_000;
                clock = 1_000_000_000; // CLOCK_BOOTTIME restarts near zero
                wrote_this_boot = false;
                ring = open().map_err(|e| TestCaseError::fail(e.to_string()))?;
                restore_like_arbd(&mut ring, boot);
                last_created = ring
                    .chunks()
                    .iter()
                    .filter(|c| c.capture.as_ref().is_some_and(|i| i.host_boot_ns == boot))
                    .map(|c| c.created_cont_ns)
                    .max();
            }
        }
        // REQ-04: every pinned chunk is present with byte-identical (SHA-256) content.
        for (id, digest) in &pinned {
            let bytes = ring
                .read_chunk(id)
                .map_err(|e| TestCaseError::fail(format!("pinned {id}: {e}")))?;
            prop_assert_eq!(&sha(&bytes), digest, "pinned chunk {} changed", id);
        }
        // Post-reboot capture: the newest new-boot write is selectable from the ring.
        if wrote_this_boot {
            let newest = format!("chunk-{seq}");
            if payloads.contains_key(&newest) {
                prop_assert!(
                    ring.descriptor(&newest).is_some(),
                    "new-boot chunk {} missing",
                    newest
                );
            }
        }
    }
    STATS_W.fetch_add(stats.writes, Ordering::Relaxed);
    STATS_P.fetch_add(stats.pins, Ordering::Relaxed);
    prop_assert_eq!(
        tree_digests(&legacy),
        legacy_before,
        "legacy schema-4 fixture modified"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(32),
        ..ProptestConfig::default()
    })]

    #[test]
    fn g2_ring_pins_survive_pressure_crash_and_reboot(ops in prop::collection::vec(op(), 1..=40)) {
        run(&ops)?;
    }
}

#[test]
fn g2_generator_is_not_vacuous() {
    g2_ring_pins_survive_pressure_crash_and_reboot();
    assert!(STATS_W.load(Ordering::Relaxed) > 0 && STATS_P.load(Ordering::Relaxed) > 0);
}
