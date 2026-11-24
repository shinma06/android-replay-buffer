//! Zone 1: Append-only immutable live ring store, Phase-1 pinning, and disk guard.
//!
//! Chunks are written append-only under `<data_dir>/ring/` and become immutable
//! once closed. Before Phase-2 bundle materialization starts, required chunk IDs
//! are durably persisted to `<data_dir>/save_jobs/<job-id>.json` via
//! [`PinJournal`] so ring eviction can never delete them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use arb_core::state::{
    PinStatus, RingBudgetPolicy, RingChunkMeta, RingChunkStatus, RingEvictionPlanner,
    RingWriteDecision,
};
use arb_core::time::{DiscontinuityEvent, DiscontinuityKind};
use arb_os::OsError;
use arb_os::fs::{DiskSpaceInfo, fsync_dir, fsync_file, query_disk_space};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StorageError;

/// Subdirectory under `data_dir` holding live ring chunk files.
pub const RING_SUBDIR: &str = "ring";

/// Subdirectory under `data_dir` holding Phase-1 durable pin journals.
pub const SAVE_JOBS_SUBDIR: &str = "save_jobs";

/// Injectable provider of host filesystem free-space statistics.
///
/// Production code uses [`OsDiskSpaceProvider`], which delegates to
/// [`arb_os::fs::query_disk_space`]. Tests inject deterministic providers to
/// verify [`StorageError::StoragePressureStop`] without filling a real disk.
pub trait DiskSpaceProvider: fmt::Debug + Send + Sync {
    /// Queries available and total bytes on the filesystem containing `path`.
    ///
    /// # Errors
    ///
    /// Returns [`OsError`] if the underlying filesystem capacity query fails.
    fn disk_space(&self, path: &Path) -> Result<DiskSpaceInfo, OsError>;
}

/// Default [`DiskSpaceProvider`] backed by [`arb_os::fs::query_disk_space`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OsDiskSpaceProvider;

impl DiskSpaceProvider for OsDiskSpaceProvider {
    fn disk_space(&self, path: &Path) -> Result<DiskSpaceInfo, OsError> {
        query_disk_space(path)
    }
}

/// Stream payload category stored inside a Zone-1 ring chunk.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingChunkKind {
    /// Encoded H.264 / video bitstream chunk.
    Video,
    /// Encoded AAC / audio stream chunk.
    Audio,
    /// Raw or framed logcat stream chunk.
    Logcat,
    /// Raw touch / input event stream chunk.
    Input,
    /// Clock triple, sync probe, and discontinuity metadata chunk.
    ClockMetadata,
}

impl RingChunkKind {
    /// Stable lowercase identifier for this chunk kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Logcat => "logcat",
            Self::Input => "input",
            Self::ClockMetadata => "clock_metadata",
        }
    }
}

/// Capture provenance and seek anchors saved atomically with closed chunk metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent presence flags for interleaved streams"
)]
pub struct CaptureChunkInfo {
    /// Approximate host boot epoch. Compared with tolerance for clock sampling jitter.
    pub host_boot_ns: i64,
    /// Whether ingest timestamps use the host continuous clock.
    #[serde(default)]
    pub host_live: bool,
    /// Physical device that produced this chunk.
    pub device_serial: String,
    /// First and last ingest stamps, independent of FIFO ordering.
    pub ingest_range: (i64, i64),
    /// The chunk contains video picture data.
    pub has_video: bool,
    /// The chunk contains a random-access picture.
    pub has_keyframe: bool,
    /// The chunk contains video parameter sets.
    pub has_video_config: bool,
    /// The chunk contains encoded audio.
    pub has_audio: bool,
    /// The chunk contains audio decoder configuration.
    pub has_audio_config: bool,
    /// The chunk contains a clock or device anchor.
    pub has_anchor: bool,
    /// Last known Android boot and paired heartbeat (device boottime, host continuous).
    pub heartbeat: Option<(String, i64, i64)>,
    /// Last known video dimensions.
    pub video_size: (u16, u16),
    /// Decoder configuration retained when earlier chunks rotate out.
    pub video_config: Option<Vec<u8>>,
    /// AAC `AudioSpecificConfig` in effect at the end of this chunk.
    pub audio_config: Option<Vec<u8>>,
}
/// Metadata describing a live or closed immutable Zone-1 ring chunk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RingChunkDescriptor {
    /// Globally unique chunk identifier within the ring directory.
    pub chunk_id: String,
    /// Stream category carried by this chunk.
    pub kind: RingChunkKind,
    /// File name relative to `<data_dir>/ring/`.
    pub file_name: String,
    /// Host `PcContinuousMono` creation timestamp in nanoseconds.
    pub created_cont_ns: i64,
    /// Approximate media/stream duration covered by this chunk in nanoseconds.
    pub duration_ns: u64,
    /// Current or finalized byte length on disk.
    pub bytes: u64,
    /// Write status (`Open` while appending, `Closed` once immutable).
    pub status: RingChunkStatus,
    /// Lowercase 64-hex SHA-256 digest populated once the chunk is closed.
    pub sha256: Option<String>,
    /// Absent for chunks written before capture provenance was recorded.
    #[serde(default)]
    pub capture: Option<CaptureChunkInfo>,
}

impl RingChunkDescriptor {
    /// Reads a closed chunk and verifies its recorded size and digest.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read or does not match its descriptor.
    pub fn read_verified(&self, ring_dir: &Path) -> Result<Vec<u8>, StorageError> {
        let path = ring_dir.join(&self.file_name);
        let bytes = fs::read(&path).map_err(|e| StorageError::io("read_ring_chunk", &path, e))?;
        if Some(self.bytes) != u64::try_from(bytes.len()).ok()
            || self
                .sha256
                .as_ref()
                .is_none_or(|digest| *digest != sha256_bytes(&bytes))
        {
            return Err(StorageError::InvalidBundle {
                path,
                reason: "ring chunk digest mismatch".into(),
            });
        }
        Ok(bytes)
    }
}

/// Durable Phase-1 pin journal record written to `<data_dir>/save_jobs/<job-id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinJournalRecord {
    /// Save job identifier owning these pins.
    pub job_id: String,
    /// Intended final bundle destination path (updatable via retarget).
    pub destination_path: PathBuf,
    /// Ordered ring chunk IDs protected from eviction until completion or discard.
    pub pinned_chunk_ids: Vec<String>,
    /// ISO-8601 UTC timestamp when Phase-1 pinning occurred.
    pub created_at_utc: String,
}

/// Durable JSON journal storing Phase-1 pinned chunk sets under `<data_dir>/save_jobs/`.
#[derive(Debug, Clone)]
pub struct PinJournal {
    jobs_dir: PathBuf,
}

impl PinJournal {
    /// Opens or creates the pin journal directory `<data_dir>/save_jobs/`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the directory cannot be created.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let jobs_dir = data_dir.as_ref().join(SAVE_JOBS_SUBDIR);
        fs::create_dir_all(&jobs_dir)
            .map_err(|source| StorageError::io("create_save_jobs_dir", &jobs_dir, source))?;
        Ok(Self { jobs_dir })
    }

    /// Returns the `<data_dir>/save_jobs/` directory path.
    #[must_use]
    pub fn jobs_dir(&self) -> &Path {
        &self.jobs_dir
    }

    /// Returns the durable journal file path for `job_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] if `job_id` is empty or contains
    /// path separators or traversal segments.
    pub fn journal_path(&self, job_id: &str) -> Result<PathBuf, StorageError> {
        validate_simple_id("job_id", job_id)?;
        Ok(self.jobs_dir.join(format!("{job_id}.json")))
    }

    /// Durably writes a Phase-1 pin journal entry using temp-file + `fsync` + rename.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] if `job_id` is invalid, or
    /// [`StorageError::Io`] / [`StorageError::Os`] / [`StorageError::Json`] on failure.
    pub fn pin(&self, record: &PinJournalRecord) -> Result<PathBuf, StorageError> {
        let final_path = self.journal_path(&record.job_id)?;
        let temp_path = self.jobs_dir.join(format!(".{}.json.tmp", record.job_id));
        let payload = serde_json::to_vec_pretty(record)
            .map_err(|source| StorageError::json("serialize_pin_journal", source))?;

        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(|source| StorageError::io("create_pin_journal_tmp", &temp_path, source))?;
        file.write_all(&payload)
            .map_err(|source| StorageError::io("write_pin_journal_tmp", &temp_path, source))?;
        fsync_file(&file)?;
        drop(file);

        arb_os::fs::replace_file(&temp_path, &final_path)
            .map_err(|source| StorageError::io("commit_pin_journal", &final_path, source))?;
        fsync_dir(&self.jobs_dir)?;
        Ok(final_path)
    }

    /// Updates only the `destination_path` of an existing pin journal while
    /// preserving all pinned chunk IDs.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if no journal exists for `job_id`,
    /// [`StorageError::InvalidArgument`] if `new_destination` is empty, or I/O errors.
    pub fn update_destination(
        &self,
        job_id: &str,
        new_destination: impl AsRef<Path>,
    ) -> Result<PinJournalRecord, StorageError> {
        let new_dest = new_destination.as_ref();
        if new_dest.as_os_str().is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "new_destination",
                reason: "destination path must not be empty".to_owned(),
            });
        }
        let mut record = self.load(job_id)?;
        record.destination_path = new_dest.to_path_buf();
        self.pin(&record)?;
        Ok(record)
    }

    /// Removes the pin journal for `job_id` once the save job completes or is discarded.
    ///
    /// Returns `Ok(true)` if a journal file existed and was removed, or `Ok(false)` if absent.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] for invalid `job_id`, or I/O / OS errors.
    pub fn unpin(&self, job_id: &str) -> Result<bool, StorageError> {
        let path = self.journal_path(job_id)?;
        match fs::remove_file(&path) {
            Ok(()) => {
                fsync_dir(&self.jobs_dir)?;
                Ok(true)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StorageError::io("remove_pin_journal", &path, source)),
        }
    }

    /// Loads the durable pin journal for `job_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if the journal file does not exist,
    /// or [`StorageError::Io`] / [`StorageError::Json`] if unreadable or malformed.
    pub fn load(&self, job_id: &str) -> Result<PinJournalRecord, StorageError> {
        let path = self.journal_path(job_id)?;
        let bytes = match fs::read(&path) {
            Ok(data) => data,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(StorageError::NotFound {
                    entity: "pin_journal",
                    id: job_id.to_owned(),
                });
            }
            Err(source) => return Err(StorageError::io("read_pin_journal", &path, source)),
        };
        serde_json::from_slice(&bytes)
            .map_err(|source| StorageError::json("deserialize_pin_journal", source))
    }

    /// Lists all active durable pin journal records under `<data_dir>/save_jobs/`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] or [`StorageError::Json`] if scanning or parsing fails.
    pub fn list_all(&self) -> Result<Vec<PinJournalRecord>, StorageError> {
        let mut records = Vec::new();
        let entries = fs::read_dir(&self.jobs_dir)
            .map_err(|source| StorageError::io("read_save_jobs_dir", &self.jobs_dir, source))?;

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|source| StorageError::io("iter_save_jobs_dir", &self.jobs_dir, source))?;
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if file_name.starts_with('.')
                || path
                    .extension()
                    .is_none_or(|ext| !ext.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            paths.push(path);
        }
        paths.sort();

        for path in paths {
            let bytes = fs::read(&path)
                .map_err(|source| StorageError::io("read_pin_journal", &path, source))?;
            let record: PinJournalRecord = serde_json::from_slice(&bytes)
                .map_err(|source| StorageError::json("deserialize_pin_journal", source))?;
            records.push(record);
        }
        Ok(records)
    }

    /// Computes the deduplicated set of all chunk IDs pinned by active save jobs.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] or [`StorageError::Json`] if journal files cannot be read.
    pub fn pinned_chunk_ids(&self) -> Result<BTreeSet<String>, StorageError> {
        let mut pinned = BTreeSet::new();
        for record in self.list_all()? {
            for chunk_id in record.pinned_chunk_ids {
                pinned.insert(chunk_id);
            }
        }
        Ok(pinned)
    }
}

/// Zone-1 append-only live ring store with Phase-1 pin protection and disk-pressure guard.
#[derive(Debug)]
pub struct RingStore<D: DiskSpaceProvider = OsDiskSpaceProvider> {
    ring_dir: PathBuf,
    pin_journal: PinJournal,
    planner: RingEvictionPlanner,
    disk_provider: D,
    chunks: BTreeMap<String, RingChunkDescriptor>,
    discontinuities: Vec<DiscontinuityEvent>,
    pressure_active: bool,
}

impl RingStore<OsDiskSpaceProvider> {
    /// Opens a Zone-1 ring store under `data_dir` using the host OS disk-space provider.
    ///
    /// Existing closed chunks with persisted `.meta.json` sidecars are reloaded
    /// so pinned chunks remain tracked across daemon restarts.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] or [`StorageError::Json`] if directories or
    /// existing chunk metadata cannot be read.
    pub fn open(
        data_dir: impl AsRef<Path>,
        policy: RingBudgetPolicy,
    ) -> Result<Self, StorageError> {
        Self::open_with_disk_provider(data_dir, policy, OsDiskSpaceProvider)
    }
}

impl<D: DiskSpaceProvider> RingStore<D> {
    /// Opens a Zone-1 ring store with a caller-supplied [`DiskSpaceProvider`].
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] or [`StorageError::Json`] if directories or
    /// persisted chunk descriptors fail to load.
    pub fn open_with_disk_provider(
        data_dir: impl AsRef<Path>,
        policy: RingBudgetPolicy,
        disk_provider: D,
    ) -> Result<Self, StorageError> {
        let data_dir = data_dir.as_ref();
        let ring_dir = data_dir.join(RING_SUBDIR);
        fs::create_dir_all(&ring_dir)
            .map_err(|source| StorageError::io("create_ring_dir", &ring_dir, source))?;
        let pin_journal = PinJournal::open(data_dir)?;

        let chunks = recover_ring_dir(&ring_dir)?;

        Ok(Self {
            ring_dir,
            pin_journal,
            planner: RingEvictionPlanner::new(policy),
            disk_provider,
            chunks,
            discontinuities: Vec::new(),
            pressure_active: false,
        })
    }

    /// Returns the `<data_dir>/ring/` directory path.
    #[must_use]
    pub fn ring_dir(&self) -> &Path {
        &self.ring_dir
    }

    /// Returns a reference to the durable Phase-1 [`PinJournal`].
    #[must_use]
    pub const fn pin_journal(&self) -> &PinJournal {
        &self.pin_journal
    }

    /// Returns the configured [`RingBudgetPolicy`].
    #[must_use]
    pub const fn policy(&self) -> RingBudgetPolicy {
        self.planner.policy()
    }

    /// Updates the live [`RingBudgetPolicy`] used for subsequent ring writes and evictions.
    pub const fn set_policy(&mut self, policy: RingBudgetPolicy) {
        self.planner = RingEvictionPlanner::new(policy);
    }

    /// Returns a reference to the injected [`DiskSpaceProvider`].
    #[must_use]
    pub const fn disk_provider(&self) -> &D {
        &self.disk_provider
    }

    /// Returns all discontinuities recorded by the ring store (such as `StoragePressureStop`)
    /// that have not been drained by [`Self::take_discontinuities`].
    ///
    /// At most one `StoragePressureStop` is recorded per pressure episode (from the first
    /// rejected write until a write is allowed again).
    #[must_use]
    pub fn discontinuities(&self) -> &[DiscontinuityEvent] {
        &self.discontinuities
    }

    /// Drains and returns the recorded discontinuities (bounded memory for long sessions).
    pub fn take_discontinuities(&mut self) -> Vec<DiscontinuityEvent> {
        std::mem::take(&mut self.discontinuities)
    }

    /// Returns `true` while a `StoragePressureStop` episode is in progress.
    #[must_use]
    pub const fn is_pressure_active(&self) -> bool {
        self.pressure_active
    }

    /// Atomically checks budget/disk pressure, evicts eligible old unpinned chunks,
    /// and writes a complete closed immutable chunk in one operation.
    ///
    /// If [`StorageError::StoragePressureStop`] or [`StorageError::RingFullPinned`]
    /// occurs, no chunk file is created and no existing chunk is evicted.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::StoragePressureStop`], [`StorageError::RingFullPinned`],
    /// [`StorageError::InvalidArgument`], or I/O / OS errors.
    pub fn write_closed_chunk(
        &mut self,
        chunk_id: impl Into<String>,
        kind: RingChunkKind,
        created_cont_ns: i64,
        duration_ns: u64,
        payload: &[u8],
    ) -> Result<(RingChunkDescriptor, Vec<String>), StorageError> {
        self.write_capture_chunk(chunk_id, kind, created_cont_ns, duration_ns, payload, None)
    }

    /// Writes a closed chunk with capture provenance and seek anchors.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::write_closed_chunk`].
    pub fn write_capture_chunk(
        &mut self,
        chunk_id: impl Into<String>,
        kind: RingChunkKind,
        created_cont_ns: i64,
        duration_ns: u64,
        payload: &[u8],
        capture: Option<CaptureChunkInfo>,
    ) -> Result<(RingChunkDescriptor, Vec<String>), StorageError> {
        let chunk_id = chunk_id.into();
        validate_simple_id("chunk_id", &chunk_id)?;
        if self.chunks.contains_key(&chunk_id) {
            return Err(StorageError::InvalidArgument {
                name: "chunk_id",
                reason: format!("chunk `{chunk_id}` already exists in ring store"),
            });
        }

        let incoming_bytes = u64::try_from(payload.len()).unwrap_or(u64::MAX);
        let evicted = self.enforce_budget_inner(incoming_bytes, created_cont_ns)?;

        let file_name = format!("{}-{chunk_id}.chunk", kind.as_str());
        let file_path = self.ring_dir.join(&file_name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file_path)
            .map_err(|source| StorageError::io("create_ring_chunk", &file_path, source))?;
        if let Err(err) = file
            .write_all(payload)
            .map_err(|source| StorageError::io("write_ring_chunk", &file_path, source))
            .and_then(|()| fsync_file(&file).map_err(StorageError::from))
        {
            drop(file);
            let _ = fs::remove_file(&file_path);
            return Err(err);
        }
        drop(file);

        let digest = sha256_bytes(payload);
        let descriptor = RingChunkDescriptor {
            chunk_id: chunk_id.clone(),
            kind,
            file_name,
            created_cont_ns,
            duration_ns,
            bytes: incoming_bytes,
            status: RingChunkStatus::Closed,
            sha256: Some(digest),
            capture,
        };
        write_chunk_meta(&self.ring_dir, &descriptor)?;

        self.chunks.insert(chunk_id, descriptor.clone());
        Ok((descriptor, evicted))
    }

    /// Durably pins `chunk_ids` for `job_id` in `<data_dir>/save_jobs/<job-id>.json`
    /// prior to Phase-2 materialization.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if any referenced chunk ID is missing from
    /// the ring store, or [`StorageError::InvalidArgument`] / I/O errors.
    pub fn pin_chunks(
        &self,
        job_id: impl Into<String>,
        destination_path: impl AsRef<Path>,
        chunk_ids: Vec<String>,
        created_at_utc: impl Into<String>,
    ) -> Result<PinJournalRecord, StorageError> {
        if chunk_ids.is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "chunk_ids",
                reason: "at least one chunk ID must be specified for pinning".to_owned(),
            });
        }
        for id in &chunk_ids {
            if !self.chunks.contains_key(id) {
                return Err(StorageError::NotFound {
                    entity: "chunk",
                    id: id.clone(),
                });
            }
        }
        let record = PinJournalRecord {
            job_id: job_id.into(),
            destination_path: destination_path.as_ref().to_path_buf(),
            pinned_chunk_ids: chunk_ids,
            created_at_utc: created_at_utc.into(),
        };
        self.pin_journal.pin(&record)?;
        Ok(record)
    }

    /// Releases the durable Phase-1 pin journal for `job_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] or I/O errors.
    pub fn unpin_job(&self, job_id: &str) -> Result<bool, StorageError> {
        self.pin_journal.unpin(job_id)
    }

    /// Evaluates disk pressure and ring quotas for `incoming_bytes`, evicting
    /// oldest unpinned closed chunks if permitted.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::StoragePressureStop`] or [`StorageError::RingFullPinned`]
    /// when writes must stop without evicting pinned or open chunks.
    pub fn enforce_budget(
        &mut self,
        incoming_bytes: u64,
        observed_cont_ns: i64,
    ) -> Result<Vec<String>, StorageError> {
        self.enforce_budget_inner(incoming_bytes, observed_cont_ns)
    }

    /// Reads the raw bytes of a ring chunk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if `chunk_id` is absent, or [`StorageError::Io`]
    /// if reading the chunk file fails.
    pub fn read_chunk(&self, chunk_id: &str) -> Result<Vec<u8>, StorageError> {
        let desc = self
            .descriptor(chunk_id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "chunk",
                id: chunk_id.to_owned(),
            })?;
        desc.read_verified(&self.ring_dir)
    }

    /// Looks up immutable chunk metadata by ID.
    #[must_use]
    pub fn descriptor(&self, id: &str) -> Option<&RingChunkDescriptor> {
        self.chunks.get(id)
    }

    /// Removes closed unpinned chunks named in `chunk_ids` (e.g., stale pre-reboot chunks),
    /// leaving any pinned chunk strictly untouched.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] if pin journals cannot be read or chunk files cannot be removed.
    pub fn purge_unpinned_chunks(
        &mut self,
        chunk_ids: &[String],
    ) -> Result<Vec<String>, StorageError> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let pinned_ids = self.pin_journal.pinned_chunk_ids()?;
        let mut purged = Vec::new();
        for chunk_id in chunk_ids {
            if pinned_ids.contains(chunk_id) {
                continue;
            }
            let is_closed = self
                .chunks
                .get(chunk_id)
                .is_some_and(|c| c.status == RingChunkStatus::Closed);
            if !is_closed {
                continue;
            }
            if let Some(chunk) = self.chunks.get(chunk_id) {
                remove_chunk_files(&self.ring_dir, chunk)?;
                self.chunks.remove(chunk_id);
                purged.push(chunk_id.clone());
            }
        }
        Ok(purged)
    }

    /// Returns the filesystem path of a tracked chunk, if present.
    #[must_use]
    pub fn chunk_path(&self, chunk_id: &str) -> Option<PathBuf> {
        self.chunks
            .get(chunk_id)
            .map(|entry| self.ring_dir.join(&entry.file_name))
    }

    /// Returns descriptors for all chunks currently tracked in the ring store,
    /// ordered by creation timestamp then `chunk_id`.
    #[must_use]
    pub fn chunks(&self) -> Vec<RingChunkDescriptor> {
        let mut list: Vec<RingChunkDescriptor> = self.chunks.values().cloned().collect();
        list.sort_by(|a, b| {
            a.created_cont_ns
                .cmp(&b.created_cont_ns)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        list
    }

    /// Returns the set of chunk IDs currently protected by durable pin journals.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] or [`StorageError::Json`] if pin journals fail to read.
    pub fn pinned_chunk_ids(&self) -> Result<BTreeSet<String>, StorageError> {
        self.pin_journal.pinned_chunk_ids()
    }

    fn enforce_budget_inner(
        &mut self,
        incoming_bytes: u64,
        observed_cont_ns: i64,
    ) -> Result<Vec<String>, StorageError> {
        let disk = self.disk_provider.disk_space(&self.ring_dir)?;
        let pinned_ids = self.pin_journal.pinned_chunk_ids()?;

        let metas: Vec<RingChunkMeta> = self
            .chunks
            .values()
            .map(|entry| RingChunkMeta {
                chunk_id: entry.chunk_id.clone(),
                created_cont_ns: entry.created_cont_ns,
                duration_ns: entry.duration_ns,
                bytes: entry.bytes,
                status: entry.status,
                pin: if pinned_ids.contains(&entry.chunk_id) {
                    PinStatus::Pinned
                } else {
                    PinStatus::Unpinned
                },
            })
            .collect();

        match self
            .planner
            .plan(&metas, incoming_bytes, disk.available_bytes)
        {
            RingWriteDecision::Allow { evict_chunk_ids } => {
                self.pressure_active = false;
                let mut evicted = Vec::with_capacity(evict_chunk_ids.len());
                for chunk_id in evict_chunk_ids {
                    // Defense-in-depth: never evict a pinned or open chunk.
                    if pinned_ids.contains(&chunk_id) {
                        continue;
                    }
                    let is_closed = self
                        .chunks
                        .get(&chunk_id)
                        .is_some_and(|c| c.status == RingChunkStatus::Closed);
                    if !is_closed {
                        continue;
                    }
                    if let Some(chunk) = self.chunks.get(&chunk_id) {
                        remove_chunk_files(&self.ring_dir, chunk)?;
                        self.chunks.remove(&chunk_id);
                        evicted.push(chunk_id);
                    }
                }
                Ok(evicted)
            }
            RingWriteDecision::StopStoragePressure {
                host_free_bytes,
                min_free_disk_bytes,
                pinned_bytes,
            } => {
                let event = DiscontinuityEvent {
                    kind: DiscontinuityKind::StoragePressureStop,
                    stream: None,
                    boot_id: None,
                    start_boottime_ns: None,
                    end_boottime_ns: None,
                    pc_cont_start_ns: observed_cont_ns,
                    pc_cont_end_ns: None,
                    delta_ns: None,
                    detail: Some(format!(
                        "host free bytes ({host_free_bytes}) below safety floor ({min_free_disk_bytes}); pinned {pinned_bytes} B preserved"
                    )),
                };
                // One discontinuity per pressure episode, not one per rejected write.
                if !self.pressure_active {
                    self.pressure_active = true;
                    self.discontinuities.push(event.clone());
                }
                Err(StorageError::StoragePressureStop {
                    host_free_bytes,
                    min_free_disk_bytes,
                    pinned_bytes,
                    event: Box::new(event),
                })
            }
            RingWriteDecision::ChunkTooLarge => Err(StorageError::InvalidArgument {
                name: "incoming_bytes",
                reason: "chunk exceeds ring byte budget".into(),
            }),
            RingWriteDecision::StopRingFullPinned {
                remaining_bytes,
                incoming_bytes,
                max_ring_bytes,
                pinned_bytes,
            } => Err(StorageError::RingFullPinned {
                remaining_bytes,
                incoming_bytes,
                max_ring_bytes,
                pinned_bytes,
            }),
            other => Err(StorageError::CorruptData {
                reason: format!("unhandled RingWriteDecision variant: {other:?}"),
            }),
        }
    }
}

pub(crate) fn validate_simple_id(name: &'static str, value: &str) -> Result<(), StorageError> {
    if value.is_empty()
        || value.starts_with('.')
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || value.contains('\0')
    {
        return Err(StorageError::InvalidArgument {
            name,
            reason: format!(
                "`{value}` must be a simple non-empty identifier without path segments"
            ),
        });
    }
    Ok(())
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

const ALL_CHUNK_KINDS: [RingChunkKind; 5] = [
    RingChunkKind::Video,
    RingChunkKind::Audio,
    RingChunkKind::Logcat,
    RingChunkKind::Input,
    RingChunkKind::ClockMetadata,
];

/// Parses `<kind>-<chunk_id>.chunk` back into its kind and chunk ID.
fn parse_chunk_file_name(name: &str) -> Option<(RingChunkKind, String)> {
    let stem = name.strip_suffix(".chunk")?;
    // Longest kind prefix first so `clock_metadata-` never matches a shorter kind.
    let mut kinds = ALL_CHUNK_KINDS;
    kinds.sort_by_key(|k| std::cmp::Reverse(k.as_str().len()));
    kinds.into_iter().find_map(|kind| {
        let id = stem.strip_prefix(kind.as_str())?.strip_prefix('-')?;
        validate_simple_id("chunk_id", id).ok()?;
        Some((kind, id.to_owned()))
    })
}

fn remove_file_if_exists(op: &'static str, path: &Path) -> Result<(), StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StorageError::io(op, path, source)),
    }
}

/// Streams `path` through SHA-256, returning `(bytes, lowercase hex digest)`.
fn hash_file(path: &Path) -> Result<(u64, String), StorageError> {
    let mut file =
        File::open(path).map_err(|source| StorageError::io("open_orphan_chunk", path, source))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|source| StorageError::io("read_orphan_chunk", path, source))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total = total.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
    }
    Ok((total, hex_lower(&hasher.finalize())))
}

/// Loads every closed chunk with a valid sidecar and repairs crash leftovers (R2 S17):
/// - `.meta.json` whose chunk file is gone → the stale sidecar is deleted;
/// - `*.chunk` without a (closed) sidecar (open at crash time, or crash between `fsync`
///   and the metadata write) → finalised as `Closed` (hashed, sidecar written) so it is
///   budgeted and evictable, or deleted when its name is not a valid chunk file name;
/// - leftover `.*.meta.json.tmp` files are deleted.
fn recover_ring_dir(
    ring_dir: &Path,
) -> Result<BTreeMap<String, RingChunkDescriptor>, StorageError> {
    let mut chunks = BTreeMap::new();
    let mut chunk_files = BTreeSet::new();
    let mut meta_files = Vec::new();
    let entries = fs::read_dir(ring_dir)
        .map_err(|source| StorageError::io("read_ring_dir", ring_dir, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| StorageError::io("iter_ring_dir", ring_dir, source))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        if name.starts_with('.') {
            if name.ends_with(".meta.json.tmp") {
                remove_file_if_exists("remove_stale_meta_tmp", &path)?;
            }
            continue;
        }
        if name.ends_with(".meta.json") {
            meta_files.push(path);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("chunk"))
        {
            chunk_files.insert(name);
        }
    }
    meta_files.sort();

    for path in meta_files {
        let raw =
            fs::read(&path).map_err(|source| StorageError::io("read_chunk_meta", &path, source))?;
        let descriptor: RingChunkDescriptor = serde_json::from_slice(&raw)
            .map_err(|source| StorageError::json("deserialize_chunk_meta", source))?;
        if !chunk_files.contains(&descriptor.file_name) {
            remove_file_if_exists("remove_orphan_chunk_meta", &path)?;
            continue;
        }
        if descriptor.status != RingChunkStatus::Closed {
            // Never trust a non-closed sidecar: re-finalise from the file below.
            continue;
        }
        chunk_files.remove(&descriptor.file_name);
        chunks.insert(descriptor.chunk_id.clone(), descriptor);
    }

    for name in chunk_files {
        let path = ring_dir.join(&name);
        let Some((kind, chunk_id)) = parse_chunk_file_name(&name) else {
            remove_file_if_exists("remove_unparsable_orphan_chunk", &path)?;
            continue;
        };
        if chunks.contains_key(&chunk_id) {
            remove_file_if_exists("remove_duplicate_orphan_chunk", &path)?;
            continue;
        }
        let (bytes, digest) = hash_file(&path)?;
        let descriptor = RingChunkDescriptor {
            chunk_id: chunk_id.clone(),
            kind,
            file_name: name,
            // Unknown after a crash: treat as oldest so it is evicted first.
            created_cont_ns: 0,
            duration_ns: 0,
            bytes,
            capture: None,
            status: RingChunkStatus::Closed,
            sha256: Some(digest),
        };
        write_chunk_meta(ring_dir, &descriptor)?;
        chunks.insert(chunk_id, descriptor);
    }
    Ok(chunks)
}

fn chunk_meta_path(ring_dir: &Path, chunk_id: &str) -> PathBuf {
    ring_dir.join(format!("{chunk_id}.meta.json"))
}

fn write_chunk_meta(ring_dir: &Path, descriptor: &RingChunkDescriptor) -> Result<(), StorageError> {
    let final_path = chunk_meta_path(ring_dir, &descriptor.chunk_id);
    let temp_path = ring_dir.join(format!(".{}.meta.json.tmp", descriptor.chunk_id));
    let payload = serde_json::to_vec_pretty(descriptor)
        .map_err(|source| StorageError::json("serialize_chunk_meta", source))?;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp_path)
        .map_err(|source| StorageError::io("create_chunk_meta_tmp", &temp_path, source))?;
    file.write_all(&payload)
        .map_err(|source| StorageError::io("write_chunk_meta_tmp", &temp_path, source))?;
    fsync_file(&file)?;
    drop(file);

    arb_os::fs::replace_file(&temp_path, &final_path)
        .map_err(|source| StorageError::io("commit_chunk_meta", &final_path, source))?;
    fsync_dir(ring_dir)?;
    Ok(())
}

fn remove_chunk_files(
    ring_dir: &Path,
    descriptor: &RingChunkDescriptor,
) -> Result<(), StorageError> {
    remove_file_if_exists(
        "evict_chunk_meta",
        &chunk_meta_path(ring_dir, &descriptor.chunk_id),
    )?;
    remove_file_if_exists("evict_ring_chunk", &ring_dir.join(&descriptor.file_name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use arb_core::state::{RingBudgetPolicy, RingChunkStatus};
    use arb_os::OsError;
    use arb_os::fs::DiskSpaceInfo;
    use tempfile::TempDir;

    use super::{DiskSpaceProvider, RingChunkKind, RingStore};
    use crate::error::StorageError;

    #[derive(Debug, Clone)]
    struct MockDiskProvider {
        available: Arc<AtomicU64>,
        total: u64,
    }

    impl MockDiskProvider {
        fn new(available: u64, total: u64) -> Self {
            Self {
                available: Arc::new(AtomicU64::new(available)),
                total,
            }
        }

        fn set_available(&self, bytes: u64) {
            self.available.store(bytes, Ordering::SeqCst);
        }
    }

    impl DiskSpaceProvider for MockDiskProvider {
        fn disk_space(&self, _path: &Path) -> Result<DiskSpaceInfo, OsError> {
            Ok(DiskSpaceInfo {
                available_bytes: self.available.load(Ordering::SeqCst),
                total_bytes: self.total,
            })
        }
    }

    #[test]
    fn append_close_evict_and_pin_protection_work_deterministically() {
        let tmp = TempDir::new().expect("temp dir must be created");
        let policy = RingBudgetPolicy {
            max_ring_bytes: 200,
            max_ring_duration_ns: 1_000_000_000,
            min_free_disk_bytes: 1_000,
        };
        let disk = MockDiskProvider::new(100_000, 200_000);
        let mut ring = RingStore::open_with_disk_provider(tmp.path(), policy, disk.clone())
            .expect("ring store open must succeed");

        let (c1, _) = ring
            .write_closed_chunk("c1", RingChunkKind::Video, 100, 100_000_000, &[1u8; 80])
            .expect("write c1");
        assert_eq!(c1.status, RingChunkStatus::Closed, "c1 must be closed");
        assert_eq!(c1.bytes, 80, "c1 byte count must equal 80");
        assert!(c1.sha256.is_some(), "closed chunk must record sha256");

        let (c2, evicted) = ring
            .write_closed_chunk("c2", RingChunkKind::Logcat, 200, 100_000_000, &[2u8; 80])
            .expect("write c2 must succeed");
        assert_eq!(c2.bytes, 80, "c2 byte count must equal 80");
        assert!(evicted.is_empty(), "budget not yet exceeded");

        // Pin c1 durably before writing c3 which exceeds 200 bytes.
        ring.pin_chunks(
            "job-1",
            tmp.path().join("dest"),
            vec!["c1".to_owned()],
            "2026-11-22T12:00:00Z",
        )
        .expect("pinning c1 must succeed");

        let (_c3, evicted) = ring
            .write_closed_chunk("c3", RingChunkKind::Video, 300, 100_000_000, &[3u8; 80])
            .expect("write c3 should evict unpinned c2 and keep pinned c1");
        assert_eq!(
            evicted,
            vec!["c2".to_owned()],
            "c2 must be evicted while pinned c1 is preserved"
        );
        assert_eq!(
            ring.read_chunk("c1")
                .expect("pinned c1 must remain readable"),
            vec![1u8; 80],
            "pinned c1 bytes must remain intact"
        );

        // Simulate low disk pressure -> StoragePressureStop without evicting c1 or c3.
        disk.set_available(500);
        let err = ring
            .write_closed_chunk("c4", RingChunkKind::Input, 400, 10_000_000, &[4u8; 10])
            .expect_err("low disk space must trigger StoragePressureStop");
        assert!(
            matches!(err, StorageError::StoragePressureStop { .. }),
            "expected StoragePressureStop, got {err:?}"
        );
        assert_eq!(
            ring.discontinuities().len(),
            1,
            "StoragePressureStop must record a discontinuity event"
        );
        assert!(
            ring.chunk_path("c1").is_some() && ring.chunk_path("c3").is_some(),
            "existing chunks must not be evicted on storage pressure"
        );
    }
}

#[cfg(test)]
#[path = "ring_r2_tests.rs"]
mod r2_tests;
