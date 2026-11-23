//! Regression tests for the R2 review fixes S17 (partial-write poisoning, deferred handle
//! release, orphan recovery) and S18 (`StoragePressureStop` deduplication).

use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arb_core::state::{RingBudgetPolicy, RingChunkStatus};
use arb_core::time::DiscontinuityKind;
use arb_os::OsError;
use arb_os::fs::DiskSpaceInfo;
use tempfile::TempDir;

use super::{DiskSpaceProvider, OpenChunkHandle, RingChunkKind, RingStore, sha256_bytes};
use crate::error::StorageError;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Disk(Arc<AtomicU64>);

impl Disk {
    fn new(available: u64) -> Self {
        Self(Arc::new(AtomicU64::new(available)))
    }
}

impl DiskSpaceProvider for Disk {
    fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
        Ok(DiskSpaceInfo {
            available_bytes: self.0.load(Ordering::SeqCst),
            total_bytes: 100 * GIB,
        })
    }
}

fn open_store(dir: &Path, disk: &Disk) -> RingStore<Disk> {
    RingStore::open_with_disk_provider(dir, RingBudgetPolicy::default(), disk.clone())
        .expect("open ring store")
}

#[test]
fn failed_append_seals_the_chunk_instead_of_leaving_a_zombie() {
    let tmp = TempDir::new().expect("tmp");
    let disk = Disk::new(50 * GIB);
    let mut ring = open_store(tmp.path(), &disk);
    ring.open_chunk("c1", RingChunkKind::Logcat, 10)
        .expect("open chunk");
    ring.append_to_chunk("c1", b"good-bytes", 1, 11)
        .expect("first append");

    // Swap in a read-only handle so the next write fails at the OS level.
    let path = ring.chunk_path("c1").expect("chunk path");
    let entry = ring.chunks.get_mut("c1").expect("entry");
    let hasher = entry.open.take().expect("open handle").hasher;
    entry.open = Some(OpenChunkHandle {
        file: File::open(&path).expect("read-only handle"),
        hasher,
    });

    let err = ring
        .append_to_chunk("c1", b"never-lands", 1, 12)
        .expect_err("write through a read-only handle must fail");
    assert!(matches!(err, StorageError::Io { .. }), "I/O error surfaced");

    let desc = ring
        .chunks()
        .into_iter()
        .find(|d| d.chunk_id == "c1")
        .expect("c1 still tracked");
    assert_eq!(
        desc.status,
        RingChunkStatus::Closed,
        "failed chunk is sealed (evictable), never a zombie Open entry"
    );
    assert_eq!(
        desc.bytes, 10,
        "descriptor keeps the last known-good length"
    );
    assert_eq!(
        fs::read(&path).expect("read chunk"),
        b"good-bytes",
        "no partial bytes beyond the known-good length"
    );
    assert!(
        matches!(
            ring.append_to_chunk("c1", b"x", 1, 13),
            Err(StorageError::InvalidArgument { .. })
        ),
        "a sealed chunk is immutable"
    );
    assert!(
        matches!(
            ring.close_chunk("c1"),
            Err(StorageError::InvalidArgument { .. })
        ),
        "a sealed chunk is already closed"
    );
}

#[test]
fn close_chunk_keeps_the_handle_until_metadata_is_durable() {
    let tmp = TempDir::new().expect("tmp");
    let disk = Disk::new(50 * GIB);
    let mut ring = open_store(tmp.path(), &disk);
    ring.open_chunk("c2", RingChunkKind::Video, 10)
        .expect("open chunk");
    ring.append_to_chunk("c2", b"part-1;", 1, 11)
        .expect("append");

    // A non-empty directory where the sidecar must land makes the metadata commit fail
    // (rename onto a non-empty directory fails on every OS).
    let blocker = ring.ring_dir().join("c2.meta.json");
    fs::create_dir_all(blocker.join("x")).expect("blocker dir");
    assert!(ring.close_chunk("c2").is_err(), "metadata commit must fail");

    // Still open: appendable and closable once the obstruction is gone.
    ring.append_to_chunk("c2", b"part-2", 1, 12)
        .expect("chunk stays appendable after a failed close");
    fs::remove_dir_all(&blocker).expect("remove blocker");
    let desc = ring.close_chunk("c2").expect("close succeeds on retry");
    assert_eq!(desc.status, RingChunkStatus::Closed, "closed");
    assert_eq!(
        desc.sha256.as_deref(),
        Some(sha256_bytes(b"part-1;part-2").as_str()),
        "digest covers every appended byte"
    );
}

#[test]
fn reopen_finalizes_orphan_chunks_and_drops_orphan_metadata() {
    let tmp = TempDir::new().expect("tmp");
    let disk = Disk::new(50 * GIB);
    {
        let mut ring = open_store(tmp.path(), &disk);
        // Crash while a chunk is open (never closed, no sidecar).
        ring.open_chunk("live", RingChunkKind::Video, 10)
            .expect("open");
        ring.append_to_chunk("live", b"crash-tail", 1, 11)
            .expect("append");
        ring.write_closed_chunk("kept", RingChunkKind::Logcat, 5, 1, b"kept")
            .expect("closed chunk");
    }
    let ring_dir = tmp.path().join("ring");
    fs::write(ring_dir.join("audio-orphan.chunk"), b"orphan-bytes").expect("orphan chunk");
    fs::write(ring_dir.join("not-a-chunk-name.chunk"), b"junk").expect("junk chunk");
    fs::write(
        ring_dir.join("ghost.meta.json"),
        br#"{"chunk_id":"ghost","kind":"video","file_name":"video-ghost.chunk","created_cont_ns":1,"duration_ns":0,"bytes":3,"status":"closed","sha256":null}"#,
    )
    .expect("orphan meta");
    fs::write(ring_dir.join(".tmp1.meta.json.tmp"), b"{").expect("stale tmp");

    let ring = open_store(tmp.path(), &disk);
    let chunks = ring.chunks();
    let find = |id: &str| chunks.iter().find(|d| d.chunk_id == id);

    let live = find("live").expect("open-at-crash chunk recovered");
    assert_eq!(live.status, RingChunkStatus::Closed, "finalised as closed");
    assert_eq!(live.bytes, 10, "byte length from disk");
    assert_eq!(
        live.sha256.as_deref(),
        Some(sha256_bytes(b"crash-tail").as_str()),
        "digest computed from disk"
    );
    let orphan = find("orphan").expect("sidecar-less chunk recovered");
    assert_eq!(
        orphan.kind,
        RingChunkKind::Audio,
        "kind parsed from file name"
    );
    assert!(find("kept").is_some(), "regular closed chunk reloaded");
    assert!(
        find("ghost").is_none(),
        "metadata without chunk is not tracked"
    );
    assert!(
        !ring_dir.join("ghost.meta.json").exists(),
        "orphan metadata deleted"
    );
    assert!(
        !ring_dir.join("not-a-chunk-name.chunk").exists(),
        "unparsable orphan deleted"
    );
    assert!(
        !ring_dir.join(".tmp1.meta.json.tmp").exists(),
        "stale temp sidecar deleted"
    );
    assert!(
        ring_dir.join("live.meta.json").is_file(),
        "recovered chunk now has a durable sidecar"
    );
}

#[test]
fn storage_pressure_stop_is_recorded_once_per_episode() {
    let tmp = TempDir::new().expect("tmp");
    let disk = Disk::new(50 * GIB);
    let mut ring = open_store(tmp.path(), &disk);
    ring.write_closed_chunk("base", RingChunkKind::Video, 1, 1, b"base")
        .expect("baseline write");

    disk.0.store(GIB, Ordering::SeqCst);
    for i in 0..50 {
        let r = ring.write_closed_chunk(format!("p{i}"), RingChunkKind::Video, 10 + i, 1, b"x");
        assert!(
            matches!(r, Err(StorageError::StoragePressureStop { .. })),
            "below the floor every write is refused"
        );
    }
    assert!(ring.is_pressure_active(), "episode in progress");
    assert_eq!(
        ring.discontinuities().len(),
        1,
        "50 rejected writes record ONE StoragePressureStop"
    );

    disk.0.store(50 * GIB, Ordering::SeqCst);
    ring.write_closed_chunk("ok", RingChunkKind::Video, 100, 1, b"ok")
        .expect("pressure cleared");
    assert!(!ring.is_pressure_active(), "episode ended");
    disk.0.store(GIB, Ordering::SeqCst);
    let _ = ring.write_closed_chunk("p-again", RingChunkKind::Video, 200, 1, b"x");
    let _ = ring.write_closed_chunk("p-again2", RingChunkKind::Video, 201, 1, b"x");

    let drained = ring.take_discontinuities();
    assert_eq!(drained.len(), 2, "one event per episode");
    assert!(
        drained
            .iter()
            .all(|d| d.kind == DiscontinuityKind::StoragePressureStop),
        "all are StoragePressureStop"
    );
    assert!(ring.discontinuities().is_empty(), "drained");
}
