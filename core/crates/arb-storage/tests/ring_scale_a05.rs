//! P1 A05 scale and complexity regression tests for [`RingStore`].
//!
//! Verifies that ring append, eviction, status (`stats`), and save-pinning remain
//! amortized O(1)–O(log N) across 1,000 / 4,000 / 8,000 chunks without O(N²) full-ring
//! metadata reconstruction or per-append `save_jobs/` directory rescans, while preserving
//! exact parity with [`RingEvictionPlanner`], stop-time pin protection, and time-span
//! monotonicity.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use arb_core::state::{
    PinStatus, RingBudgetPolicy, RingChunkMeta, RingChunkStatus, RingEvictionPlanner,
    RingWriteDecision,
};
use arb_os::OsError;
use arb_os::fs::DiskSpaceInfo;
use arb_storage::error::StorageError;
use arb_storage::ring::{DiskSpaceProvider, RingChunkDescriptor, RingChunkKind, RingStore};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const GIB: u64 = 1024 * 1024 * 1024;
const STEP_NS: u64 = 100_000_000;
const BASE_CONT_NS: i64 = 1_000_000_000;
const CHUNK_BYTES: usize = 64;
const APPEND_BURST: usize = 32;

#[derive(Debug, Clone)]
struct FakeDisk(Arc<AtomicU64>);

impl FakeDisk {
    fn new(available: u64) -> Self {
        Self(Arc::new(AtomicU64::new(available)))
    }

    fn set(&self, available: u64) {
        self.0.store(available, Ordering::SeqCst);
    }
}

impl DiskSpaceProvider for FakeDisk {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        Ok(DiskSpaceInfo {
            available_bytes: self.0.load(Ordering::SeqCst),
            total_bytes: 100 * GIB,
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Seeds `count` valid closed chunks + `.meta.json` sidecars directly under `<data_dir>/ring/`
/// so `RingStore::open_with_disk_provider` recovers them through the real production path.
fn seed_closed_chunks_on_disk(data_dir: &Path, count: usize) -> Vec<(String, String)> {
    let ring_dir = data_dir.join("ring");
    fs::create_dir_all(&ring_dir).expect("create ring dir");

    let mut payload = [0u8; CHUNK_BYTES];
    let mut seeded = Vec::with_capacity(count);
    for i in 0..count {
        let idx_u64 = u64::try_from(i).expect("index fits u64");
        let id = format!("seed-{i:06}");
        let file_name = format!("video-{id}.chunk");
        payload[..8].copy_from_slice(&idx_u64.to_le_bytes());
        payload[8..].fill(u8::try_from(i & 0xFF).expect("low byte fits u8"));
        let digest = sha256_hex(&payload);
        let created_cont_ns = BASE_CONT_NS + i64::try_from(idx_u64 * STEP_NS).expect("ts fits i64");

        fs::write(ring_dir.join(&file_name), payload).expect("write seeded chunk");
        let desc = RingChunkDescriptor {
            chunk_id: id.clone(),
            kind: RingChunkKind::Video,
            file_name,
            created_cont_ns,
            duration_ns: STEP_NS,
            bytes: u64::try_from(CHUNK_BYTES).expect("bytes fits u64"),
            status: RingChunkStatus::Closed,
            sha256: Some(digest.clone()),
            capture: None,
        };
        let meta_json = serde_json::to_vec(&desc).expect("serialize chunk meta");
        fs::write(ring_dir.join(format!("{id}.meta.json")), meta_json).expect("write meta");
        seeded.push((id, digest));
    }
    seeded
}

struct ScaleSample {
    initial_chunks: usize,
    budget_checks_during_ops: u64,
    candidates_scanned_during_ops: u64,
    chunks_evicted_during_ops: u64,
    pin_reloads_during_ops: u64,
    in_memory_budget_and_status_us: u128,
}

fn assert_time_span_and_pressure_pins(
    ring: &mut RingStore<FakeDisk>,
    disk: &FakeDisk,
    seeded: &[(String, String)],
    pinned_indices: &[usize],
) {
    let chunk_bytes_u64 = u64::try_from(CHUNK_BYTES).expect("bytes fits u64");
    let ordered = ring.chunks();
    let final_stats = ring.stats().expect("final stats");
    assert_eq!(
        ordered.len(),
        final_stats.chunk_count,
        "ordered chunks length must match O(1) stats chunk_count"
    );
    assert!(
        ordered.windows(2).all(|w| {
            (w[0].created_cont_ns, &w[0].chunk_id) < (w[1].created_cont_ns, &w[1].chunk_id)
        }),
        "ring chunks must stay strictly ordered by (created_cont_ns, chunk_id)"
    );
    assert_eq!(
        final_stats.oldest_created_cont_ns,
        ordered.first().map(|c| c.created_cont_ns),
        "oldest timestamp must match first ordered chunk"
    );
    assert_eq!(
        final_stats.newest_created_cont_ns,
        ordered.last().map(|c| c.created_cont_ns),
        "newest timestamp must match last ordered chunk"
    );
    let exact_bytes: u64 = ordered.iter().map(|c| c.bytes).sum();
    let exact_duration: u64 = ordered.iter().map(|c| c.duration_ns).sum();
    assert_eq!(
        final_stats.total_bytes, exact_bytes,
        "incremental total_bytes must equal exact sum"
    );
    assert_eq!(
        final_stats.total_duration_ns, exact_duration,
        "incremental total_duration_ns must equal exact sum"
    );

    disk.set(GIB / 2);
    let err = ring
        .write_capture_chunk(
            "pressure-stop",
            RingChunkKind::Video,
            BASE_CONT_NS + 999_999_999_999,
            STEP_NS,
            &[0xAA; CHUNK_BYTES],
            None,
        )
        .expect_err("must stop under storage pressure");
    match err {
        StorageError::StoragePressureStop { pinned_bytes, .. } => {
            assert_eq!(
                pinned_bytes,
                u64::try_from(pinned_indices.len()).expect("u64") * chunk_bytes_u64,
                "StoragePressureStop must report exact pinned bytes"
            );
        }
        other => panic!("expected StoragePressureStop, got {other:?}"),
    }
    for &idx in pinned_indices {
        let (ref id, ref expected_sha) = seeded[idx];
        let bytes = ring
            .read_chunk(id)
            .expect("pinned chunk must survive StoragePressureStop");
        assert_eq!(
            &sha256_hex(&bytes),
            expected_sha,
            "pinned chunk `{id}` digest must stay intact after pressure stop"
        );
    }
}

fn run_append_burst_and_transient_save(
    ring: &mut RingStore<FakeDisk>,
    tmp_dir: &Path,
    seeded: &[(String, String)],
    pinned_indices: &[usize],
    pinned_set: &BTreeSet<String>,
    policy: RingBudgetPolicy,
) {
    let initial_chunks = seeded.len();
    let initial_reloads = ring.work_counters().pin_journal_disk_reloads;
    let mut all_evicted = Vec::new();
    for step in 0..APPEND_BURST {
        let global_idx = u64::try_from(initial_chunks + step).expect("u64");
        let ts = BASE_CONT_NS + i64::try_from(global_idx * STEP_NS).expect("i64");
        let mut payload = [0u8; CHUNK_BYTES];
        payload[..8].copy_from_slice(&global_idx.to_le_bytes());
        let (_desc, evicted) = ring
            .write_capture_chunk(
                format!("live-{step:04}"),
                RingChunkKind::Video,
                ts,
                STEP_NS,
                &payload,
                None,
            )
            .expect("live append");
        for ev in &evicted {
            assert!(
                !pinned_set.contains(ev),
                "pinned chunk `{ev}` must never be evicted at scale {initial_chunks}"
            );
        }
        all_evicted.extend(evicted);

        let st = ring.stats().expect("status during append");
        assert!(
            st.total_bytes <= policy.max_ring_bytes,
            "ring bytes {} exceeded cap {} at scale {initial_chunks}",
            st.total_bytes,
            policy.max_ring_bytes
        );
        assert_eq!(
            st.pinned_chunk_count,
            pinned_indices.len(),
            "pinned chunk count must remain constant during live appends"
        );
    }
    assert_eq!(
        ring.work_counters().pin_journal_disk_reloads,
        initial_reloads,
        "steady-state eviction must reuse the pin snapshot"
    );
    assert!(
        all_evicted.len() >= 16,
        "expected steady-state evictions (>=16), got {} at scale {initial_chunks}",
        all_evicted.len()
    );
    assert_eq!(
        all_evicted[0], "seed-000003",
        "oldest unpinned chunk must be evicted first"
    );
    assert_eq!(
        all_evicted[1], "seed-000004",
        "second oldest unpinned chunk must be evicted second"
    );

    for &idx in pinned_indices {
        let (ref id, ref expected_sha) = seeded[idx];
        let bytes = ring.read_chunk(id).expect("pinned chunk readable");
        assert_eq!(&sha256_hex(&bytes), expected_sha, "pinned digest intact");
    }
    ring.pin_chunks(
        "save-transient",
        tmp_dir.join("transient"),
        vec!["live-0000".to_owned(), "live-0001".to_owned()],
        "2026-11-25T00:00:01Z",
    )
    .expect("pin transient job");
    assert_eq!(
        ring.stats()
            .expect("stats with transient pin")
            .pinned_chunk_count,
        pinned_indices.len() + 2,
        "transient pin must increment tracked pinned_chunk_count"
    );
    assert!(
        ring.unpin_job("save-transient").expect("unpin transient"),
        "unpin_job must remove transient pin journal"
    );
    assert_eq!(
        ring.stats().expect("stats after unpin").pinned_chunk_count,
        pinned_indices.len(),
        "unpin_job must restore tracked pinned_chunk_count"
    );
}

fn run_scale_case(initial_chunks: usize) -> ScaleSample {
    let tmp = TempDir::new().expect("tempdir");
    let seeded = seed_closed_chunks_on_disk(tmp.path(), initial_chunks);
    let chunk_bytes_u64 = u64::try_from(CHUNK_BYTES).expect("bytes fits u64");

    let cap_chunks = u64::try_from(initial_chunks + 16).expect("cap fits u64");
    let policy = RingBudgetPolicy {
        max_ring_bytes: cap_chunks * chunk_bytes_u64,
        max_ring_duration_ns: cap_chunks * STEP_NS,
        min_free_disk_bytes: GIB,
    };
    let disk = FakeDisk::new(50 * GIB);
    let mut ring = RingStore::open_with_disk_provider(tmp.path(), policy, disk.clone())
        .expect("open seeded ring");

    let pinned_indices: Vec<_> = (0..initial_chunks)
        .filter(|&i| i < 3 || (i >= 5 && i % 2 == 1))
        .collect();
    let pinned_ids: Vec<String> = pinned_indices
        .iter()
        .map(|&idx| seeded[idx].0.clone())
        .collect();
    let pinned_set: BTreeSet<String> = pinned_ids.iter().cloned().collect();
    ring.pin_chunks(
        "save-pin-mix",
        tmp.path().join("out-bundle"),
        pinned_ids.clone(),
        "2026-11-25T00:00:00Z",
    )
    .expect("pin mixed chunks");

    let initial_stats = ring.stats().expect("initial stats");
    assert_eq!(
        initial_stats.chunk_count, initial_chunks,
        "initial chunk_count must match seeded count"
    );
    assert_eq!(
        initial_stats.total_bytes,
        u64::try_from(initial_chunks).expect("u64") * chunk_bytes_u64,
        "initial total_bytes must match seeded bytes"
    );
    assert_eq!(
        initial_stats.pinned_chunk_count,
        pinned_ids.len(),
        "initial pinned_chunk_count must match pin journal"
    );
    assert_eq!(
        initial_stats.pinned_bytes,
        u64::try_from(pinned_ids.len()).expect("u64") * chunk_bytes_u64,
        "initial pinned_bytes must match pinned chunk sizes"
    );

    let before = ring.work_counters();
    run_append_burst_and_transient_save(
        &mut ring,
        tmp.path(),
        &seeded,
        &pinned_indices,
        &pinned_set,
        policy,
    );

    let bench_start = Instant::now();
    for i in 0..i64::try_from(initial_chunks).expect("scale fits i64") {
        let ev = ring
            .enforce_budget(0, BASE_CONT_NS + i)
            .expect("zero-byte budget check");
        assert_eq!(
            ev,
            Vec::<String>::new(),
            "zero-byte budget check must not evict within cap"
        );
        let st = ring.stats().expect("stats");
        assert!(
            st.chunk_count >= initial_chunks,
            "chunk_count {} must stay >= {initial_chunks}",
            st.chunk_count
        );
    }
    let in_memory_budget_and_status_us = bench_start.elapsed().as_micros();
    let after = ring.work_counters();

    assert_time_span_and_pressure_pins(&mut ring, &disk, &seeded, &pinned_indices);

    ScaleSample {
        initial_chunks,
        budget_checks_during_ops: after.budget_checks - before.budget_checks,
        candidates_scanned_during_ops: after.eviction_candidates_scanned
            - before.eviction_candidates_scanned,
        chunks_evicted_during_ops: after.chunks_evicted - before.chunks_evicted,
        pin_reloads_during_ops: after.pin_journal_disk_reloads - before.pin_journal_disk_reloads,
        in_memory_budget_and_status_us,
    }
}

#[test]
fn a05_ring_append_status_save_scales_without_quadratic_scans() {
    let s1k = run_scale_case(1_000);
    let s4k = run_scale_case(4_000);
    let s8k = run_scale_case(8_000);

    for sample in [&s1k, &s4k, &s8k] {
        assert_eq!(
            sample.pin_reloads_during_ops, 2,
            "scale {}: hot appends and status checks must reload only after the transient pin and unpin",
            sample.initial_chunks
        );
        assert_eq!(
            sample.candidates_scanned_during_ops,
            sample.chunks_evicted_during_ops,
            "scale {}: eviction candidates scanned ({}) must equal evicted chunks ({})",
            sample.initial_chunks,
            sample.candidates_scanned_during_ops,
            sample.chunks_evicted_during_ops
        );
        assert!(
            sample.candidates_scanned_during_ops <= sample.budget_checks_during_ops,
            "scale {}: amortized scan per budget check must be <= 1",
            sample.initial_chunks
        );
    }

    assert_eq!(
        s8k.candidates_scanned_during_ops, s4k.candidates_scanned_during_ops,
        "scan count must be independent of total ring size (4k vs 8k)"
    );
    let max_allowed_8k_us = s1k.in_memory_budget_and_status_us.saturating_mul(32) + 2_000;
    assert!(
        s8k.in_memory_budget_and_status_us <= max_allowed_8k_us,
        "8k in-memory budget+status ({} us) grew too fast vs 1k ({} us)",
        s8k.in_memory_budget_and_status_us,
        s1k.in_memory_budget_and_status_us
    );
}

#[test]
fn a05_incremental_planner_matches_reference_eviction_planner_and_cloned_pin_journal() {
    let tmp = TempDir::new().expect("tempdir");
    let policy = RingBudgetPolicy {
        max_ring_bytes: 300,
        max_ring_duration_ns: 400,
        min_free_disk_bytes: 1_000,
    };
    let disk = FakeDisk::new(50_000);
    let mut ring = RingStore::open_with_disk_provider(tmp.path(), policy, disk).expect("open ring");
    let cloned_journal = ring.pin_journal().clone();
    let reference = RingEvictionPlanner::new(policy);

    let snapshot_metas = |r: &RingStore<FakeDisk>| -> Vec<RingChunkMeta> {
        let pinned = r.pinned_chunk_ids().expect("pinned ids");
        r.chunks()
            .into_iter()
            .map(|d| RingChunkMeta {
                chunk_id: d.chunk_id.clone(),
                created_cont_ns: d.created_cont_ns,
                duration_ns: d.duration_ns,
                bytes: d.bytes,
                status: d.status,
                pin: if pinned.contains(&d.chunk_id) {
                    PinStatus::Pinned
                } else {
                    PinStatus::Unpinned
                },
            })
            .collect()
    };

    for i in 1..=3_i64 {
        let (_, ev) = ring
            .write_closed_chunk(
                format!("c{i}"),
                RingChunkKind::Video,
                i * 100,
                100,
                &[1; 100],
            )
            .expect("write");
        assert_eq!(
            ev,
            Vec::<String>::new(),
            "first 3 chunks fit within 300-byte budget"
        );
    }

    ring.pin_chunks(
        "job-a",
        tmp.path().join("dest-a"),
        vec!["c1".to_owned(), "c2".to_owned()],
        "2026-11-25T00:00:00Z",
    )
    .expect("pin c1,c2");

    let metas = snapshot_metas(&ring);
    let expected = reference.plan(&metas, 150, 50_000);
    assert!(
        matches!(
            expected,
            RingWriteDecision::StopRingFullPinned {
                remaining_bytes: 200,
                incoming_bytes: 150,
                max_ring_bytes: 300,
                pinned_bytes: 200,
            }
        ),
        "reference planner must return RingFullPinned, got {expected:?}"
    );
    let actual_err = ring
        .write_closed_chunk("c4", RingChunkKind::Video, 400, 100, &[4; 150])
        .expect_err("must hit RingFullPinned");
    assert!(
        matches!(
            actual_err,
            StorageError::RingFullPinned {
                remaining_bytes: 200,
                incoming_bytes: 150,
                max_ring_bytes: 300,
                pinned_bytes: 200,
            }
        ),
        "RingStore must match reference RingFullPinned fields, got {actual_err:?}"
    );

    assert!(
        cloned_journal.unpin("job-a").expect("unpin via clone"),
        "unpin via cloned PinJournal must remove job-a"
    );

    let metas_after_unpin = snapshot_metas(&ring);
    let expected_after_unpin = reference.plan(&metas_after_unpin, 150, 50_000);
    let RingWriteDecision::Allow { evict_chunk_ids } = expected_after_unpin else {
        panic!("expected Allow after unpin, got {expected_after_unpin:?}");
    };
    let (_c4, actual_evicted) = ring
        .write_closed_chunk("c4", RingChunkKind::Video, 400, 100, &[4; 150])
        .expect("write after unpin via clone");
    assert_eq!(
        actual_evicted, evict_chunk_ids,
        "incremental eviction must match RingEvictionPlanner bit-for-bit"
    );
}

#[test]
fn external_journal_edits_survive_local_pin_and_eviction() {
    let tmp = TempDir::new().unwrap();
    let policy = RingBudgetPolicy {
        max_ring_bytes: 3,
        max_ring_duration_ns: 100,
        min_free_disk_bytes: 0,
    };
    let mut ring =
        RingStore::open_with_disk_provider(tmp.path(), policy, FakeDisk::new(GIB)).unwrap();
    for (i, id) in ["a", "b", "c"].iter().enumerate() {
        ring.write_closed_chunk(
            *id,
            RingChunkKind::Video,
            i64::try_from(i).unwrap(),
            1,
            &[1],
        )
        .unwrap();
    }
    let first = ring
        .pin_chunks("local", tmp.path(), vec!["b".into()], "now")
        .unwrap();
    let mut foreign = first.clone();
    foreign.job_id = "external".into();
    foreign.pinned_chunk_ids = vec!["a".into()];
    let external = ring.pin_journal().journal_path("external").unwrap();
    fs::write(&external, serde_json::to_vec(&foreign).unwrap()).unwrap();
    ring.pin_journal().pin(&first).unwrap();
    assert_eq!(ring.stats().unwrap().pinned_chunk_count, 2);
    foreign.pinned_chunk_ids.push("c".into());
    fs::write(external, serde_json::to_vec(&foreign).unwrap()).unwrap();
    assert_eq!(ring.stats().unwrap().pinned_chunk_count, 3);
    assert!(matches!(
        ring.enforce_budget(1, 10),
        Err(StorageError::RingFullPinned { .. })
    ));
    assert_eq!(
        ring.purge_unpinned_chunks(&foreign.pinned_chunk_ids)
            .unwrap(),
        Vec::<String>::new()
    );
}
