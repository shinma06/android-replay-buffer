//! Zone 3: `SQLite` WAL catalog, viewer/job leases, save-job lifecycle, and deferred GC.
//!
//! Manages mutable metadata (`bundles`, `leases`, `save_jobs`, `edit_sessions`)
//! completely outside immutable Zone-2 WORM bundles:
//! - Logical delete (`trash_bundle`) only flips `bundles.state = 'trashed'` and
//!   never touches files on disk.
//! - Deferred garbage collection (`gc_trashed_bundles`) physically removes ONLY
//!   managed non-legacy bundles in `'trashed'` state with zero active leases and
//!   zero active save jobs.
//! - Startup crash recovery (`recover_on_startup`) removes orphan `.staging-*`
//!   directories, reaps leases belonging to dead PIDs via [`arb_os::fs::is_pid_alive`],
//!   and transitions interrupted save jobs into `FailedRetryable` with pins intact.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use arb_core::state::{BundleLifecycleState, SaveJob, SaveJobEvent, SaveJobState};
use arb_os::fs::is_pid_alive;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::ring::{PinJournal, PinJournalRecord};

static LEASE_SEQ: AtomicU64 = AtomicU64::new(1);

const CATALOG_PRAGMAS: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
";

const CATALOG_DDL: &str = "
CREATE TABLE IF NOT EXISTS bundles (
  bundle_id TEXT PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  format_version TEXT NOT NULL,
  state TEXT NOT NULL,
  trashed_at_utc TEXT,
  parent_bundle_id TEXT,
  created_at_utc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS leases (
  lease_id TEXT PRIMARY KEY,
  bundle_id TEXT NOT NULL REFERENCES bundles(bundle_id),
  holder_kind TEXT NOT NULL,
  holder_pid INTEGER NOT NULL,
  last_heartbeat_utc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS save_jobs (
  job_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  destination_path TEXT NOT NULL,
  pinned_chunks_json TEXT NOT NULL,
  error_message TEXT,
  created_at_utc TEXT NOT NULL,
  updated_at_utc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edit_sessions (
  session_id TEXT PRIMARY KEY,
  bundle_id TEXT NOT NULL REFERENCES bundles(bundle_id),
  in_frame_index INTEGER,
  out_frame_index INTEGER,
  playhead_frame_index INTEGER,
  av_offset_override_ms INTEGER,
  updated_at_utc TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_bundles_state ON bundles(state);
CREATE INDEX IF NOT EXISTS idx_leases_bundle_id ON leases(bundle_id);
CREATE INDEX IF NOT EXISTS idx_edit_sessions_bundle_id ON edit_sessions(bundle_id);
";

/// Stored format tag in `bundles.format_version`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFormatVersion {
    /// Managed or external immutable WORM v2 bundle.
    V2,
    /// Legacy schema 3 or schema 4 folder opened strictly read-only.
    LegacyRo,
}

impl CatalogFormatVersion {
    const fn as_str(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::LegacyRo => "legacy_ro",
        }
    }

    fn from_db(value: &str) -> Result<Self, StorageError> {
        match value {
            "v2" => Ok(Self::V2),
            "legacy_ro" => Ok(Self::LegacyRo),
            other => Err(StorageError::CorruptData {
                reason: format!("unknown bundles.format_version `{other}`"),
            }),
        }
    }
}

/// Lease holder category recorded in `leases.holder_kind`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseHolderKind {
    /// Embedded IDE plugin / standalone viewer reading the bundle.
    IdeViewer,
    /// Background export or non-destructive trim job reading the bundle.
    ExportJob,
    /// Command-line `arb` / `replay` client reading the bundle.
    Cli,
}

impl LeaseHolderKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::IdeViewer => "ide_viewer",
            Self::ExportJob => "export_job",
            Self::Cli => "cli",
        }
    }

    fn from_db(value: &str) -> Result<Self, StorageError> {
        match value {
            "ide_viewer" => Ok(Self::IdeViewer),
            "export_job" => Ok(Self::ExportJob),
            "cli" => Ok(Self::Cli),
            other => Err(StorageError::CorruptData {
                reason: format!("unknown leases.holder_kind `{other}`"),
            }),
        }
    }
}

/// Parameters for registering a newly committed or discovered bundle in the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBundleEntry {
    /// Globally unique bundle identifier.
    pub bundle_id: String,
    /// Filesystem path to the bundle directory.
    pub path: PathBuf,
    /// Format version (`V2` or `LegacyRo`).
    pub format_version: CatalogFormatVersion,
    /// Parent bundle ID for non-destructive trims, if any.
    pub parent_bundle_id: Option<String>,
    /// Creation timestamp in ISO-8601 UTC format.
    pub created_at_utc: String,
}

/// Persisted bundle row from the `bundles` catalog table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    /// Globally unique bundle identifier.
    pub bundle_id: String,
    /// Filesystem path to the bundle directory.
    pub path: PathBuf,
    /// Format version (`V2` or `LegacyRo`).
    pub format_version: CatalogFormatVersion,
    /// Lifecycle state (`Active`, `Trashed`, or `Purging`).
    pub state: BundleLifecycleState,
    /// ISO-8601 UTC timestamp when logically trashed, if `Trashed`.
    pub trashed_at_utc: Option<String>,
    /// Parent bundle ID when created by non-destructive trim.
    pub parent_bundle_id: Option<String>,
    /// ISO-8601 UTC creation timestamp.
    pub created_at_utc: String,
}

/// Persisted reader/job lease row from the `leases` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRecord {
    /// Unique lease identifier.
    pub lease_id: String,
    /// Referenced bundle identifier.
    pub bundle_id: String,
    /// Kind of client holding the lease.
    pub holder_kind: LeaseHolderKind,
    /// Host OS process ID of the lease holder.
    pub holder_pid: u32,
    /// ISO-8601 UTC timestamp of the last lease heartbeat.
    pub last_heartbeat_utc: String,
}

/// Persisted save job row wrapping [`arb_core::state::SaveJob`] with timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSaveJob {
    /// Core state-machine representation including `pinned_chunk_ids`.
    pub job: SaveJob,
    /// ISO-8601 UTC creation timestamp.
    pub created_at_utc: String,
    /// ISO-8601 UTC last-update timestamp.
    pub updated_at_utc: String,
}

/// Non-destructive editor state stored outside immutable Zone-2 bundles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSession {
    /// Unique edit session identifier.
    pub session_id: String,
    /// Referenced immutable bundle identifier.
    pub bundle_id: String,
    /// Optional inclusive trim start frame index.
    pub in_frame_index: Option<u64>,
    /// Optional inclusive trim end frame index.
    pub out_frame_index: Option<u64>,
    /// Optional viewer playhead frame index.
    pub playhead_frame_index: Option<u64>,
    /// Optional manual A/V sync offset override in milliseconds.
    pub av_offset_override_ms: Option<i64>,
    /// ISO-8601 UTC timestamp of the last edit update.
    pub updated_at_utc: String,
}

/// Summary report returned by [`Catalog::gc_trashed_bundles`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcReport {
    /// Managed trashed bundle IDs physically deleted and removed from the catalog.
    pub purged_bundle_ids: Vec<String>,
    /// Trashed bundle IDs retained because active leases were present.
    pub skipped_leased_bundle_ids: Vec<String>,
    /// Trashed bundle IDs retained because active save jobs targeted their path.
    pub skipped_active_job_bundle_ids: Vec<String>,
    /// Trashed bundle IDs skipped because they are legacy or outside `managed_bundles_root`.
    pub skipped_unmanaged_or_legacy_bundle_ids: Vec<String>,
    /// Trashed bundle IDs skipped because `trashed_at_utc > cutoff_utc` (or its timestamp
    /// could not be parsed, the safe direction).
    pub skipped_in_grace_bundle_ids: Vec<String>,
    /// Bundles whose purge failed this run (e.g. a Windows sharing violation from an AV
    /// scanner). They stay `Purging` and are retried by the next GC run.
    pub failed: Vec<GcFailure>,
}

/// One bundle whose physical purge failed during [`Catalog::gc_trashed_bundles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcFailure {
    /// Bundle identifier left in `Purging` for retry.
    pub bundle_id: String,
    /// Human-readable failure description.
    pub error: String,
}

/// Summary returned by [`Catalog::reconcile_pins`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PinReconcileReport {
    /// Pin journals released because their job is `Completed`, `Discarded`, or unknown to the
    /// catalog (crash between `complete_job`/`discard_job` and `unpin_job`).
    pub released_job_ids: Vec<String>,
    /// Active/retryable catalog jobs whose journal was missing; the journal was re-created
    /// from the catalog's pinned chunk IDs so surviving chunks cannot be evicted before the
    /// job is retried, retargeted or discarded.
    pub repinned_job_ids: Vec<String>,
}

/// Pin journal state per catalog job, as seen by [`Catalog::reconcile_pins`].
const ACTIVE_JOB_STATES: [SaveJobState; 4] = [
    SaveJobState::Pinned,
    SaveJobState::Draining,
    SaveJobState::Materializing,
    SaveJobState::FailedRetryable,
];

/// Summary report returned by [`Catalog::recover_on_startup`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// Orphan `.staging-*` directories removed from `managed_bundles_root`.
    pub removed_staging_dirs: Vec<PathBuf>,
    /// Lease IDs reaped because their `holder_pid` was no longer alive.
    pub reaped_lease_ids: Vec<String>,
    /// Interrupted save job IDs transitioned to `FailedRetryable` with pins preserved.
    pub recovered_job_ids: Vec<String>,
    /// Previously interrupted `Purging` managed bundle IDs cleaned up on restart.
    pub completed_purging_bundle_ids: Vec<String>,
}

/// Zone-3 `SQLite` WAL catalog managing bundles, leases, save jobs, and edit sessions.
#[derive(Debug)]
pub struct Catalog {
    conn: Mutex<Connection>,
    managed_bundles_root: PathBuf,
}

impl Catalog {
    /// Opens or initializes `catalog.sqlite` in WAL mode with foreign keys enabled.
    ///
    /// Only bundles located strictly inside `managed_bundles_root` (and not marked
    /// [`CatalogFormatVersion::LegacyRo`]) can ever be physically removed by GC.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if parent directories cannot be created, or
    /// [`StorageError::Sqlite`] if opening or initializing the database fails.
    pub fn open(
        db_path: impl AsRef<Path>,
        managed_bundles_root: impl AsRef<Path>,
    ) -> Result<Self, StorageError> {
        let db_path = db_path.as_ref();
        if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .map_err(|source| StorageError::io("create_catalog_parent_dir", parent, source))?;
        }
        let managed_root = managed_bundles_root.as_ref();
        fs::create_dir_all(managed_root).map_err(|source| {
            StorageError::io("create_managed_bundles_root", managed_root, source)
        })?;
        let canonical_root =
            fs::canonicalize(managed_root).unwrap_or_else(|_| normalize_lexical(managed_root));

        let conn = Connection::open(db_path)
            .map_err(|source| StorageError::sqlite("open_catalog_db", source))?;
        conn.execute_batch(CATALOG_PRAGMAS)
            .map_err(|source| StorageError::sqlite("configure_catalog_pragmas", source))?;
        conn.execute_batch(CATALOG_DDL)
            .map_err(|source| StorageError::sqlite("create_catalog_schema", source))?;

        Ok(Self {
            conn: Mutex::new(conn),
            managed_bundles_root: canonical_root,
        })
    }

    /// Returns the managed bundles root directory owned by this catalog.
    #[must_use]
    pub fn managed_bundles_root(&self) -> &Path {
        &self.managed_bundles_root
    }

    /// Registers a bundle in `bundles` with initial state `Active`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] for empty `bundle_id` or `path`,
    /// or [`StorageError::Sqlite`] on constraint violations or DB failures.
    pub fn register_bundle(&self, entry: &NewBundleEntry) -> Result<BundleEntry, StorageError> {
        if entry.bundle_id.trim().is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "bundle_id",
                reason: "bundle_id must not be empty".to_owned(),
            });
        }
        if entry.path.as_os_str().is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "path",
                reason: "bundle path must not be empty".to_owned(),
            });
        }
        let path_str = entry.path.to_string_lossy().into_owned();
        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO bundles (
                bundle_id, path, format_version, state, trashed_at_utc,
                parent_bundle_id, created_at_utc
            ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                entry.bundle_id,
                path_str,
                entry.format_version.as_str(),
                bundle_state_to_str(BundleLifecycleState::Active),
                entry.parent_bundle_id,
                entry.created_at_utc,
            ],
        )
        .map_err(|source| StorageError::sqlite("register_bundle", source))?;

        Ok(BundleEntry {
            bundle_id: entry.bundle_id.clone(),
            path: entry.path.clone(),
            format_version: entry.format_version,
            state: BundleLifecycleState::Active,
            trashed_at_utc: None,
            parent_bundle_id: entry.parent_bundle_id.clone(),
            created_at_utc: entry.created_at_utc.clone(),
        })
    }

    /// Fetches a single bundle entry by `bundle_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] or [`StorageError::CorruptData`].
    pub fn bundle(&self, bundle_id: &str) -> Result<Option<BundleEntry>, StorageError> {
        let conn = self.lock_conn();
        fetch_bundle(&conn, bundle_id)
    }

    /// Lists cataloged bundles, optionally filtered by lifecycle `state_filter`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] or [`StorageError::CorruptData`].
    pub fn list_bundles(
        &self,
        state_filter: Option<BundleLifecycleState>,
    ) -> Result<Vec<BundleEntry>, StorageError> {
        let conn = self.lock_conn();
        let state_str = state_filter.map(bundle_state_to_str);
        let mut stmt = conn
            .prepare(
                "SELECT bundle_id, path, format_version, state, trashed_at_utc,
                        parent_bundle_id, created_at_utc
                 FROM bundles
                 WHERE (?1 IS NULL OR state = ?1)
                 ORDER BY created_at_utc ASC, bundle_id ASC",
            )
            .map_err(|source| StorageError::sqlite("prepare_list_bundles", source))?;

        let rows = stmt
            .query_map(params![state_str], read_bundle_sql_row)
            .map_err(|source| StorageError::sqlite("list_bundles", source))?;

        let mut results = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_bundle_row", source))?;
            results.push(decode_bundle_row(raw)?);
        }
        Ok(results)
    }

    /// Logically moves an `Active` bundle into `Trashed` without touching disk files.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if the bundle is absent,
    /// [`StorageError::StateTransition`] if it is not `Active`, or [`StorageError::Sqlite`].
    pub fn trash_bundle(
        &self,
        bundle_id: &str,
        trashed_at_utc: &str,
    ) -> Result<BundleEntry, StorageError> {
        let conn = self.lock_conn();
        let mut entry = fetch_bundle(&conn, bundle_id)?.ok_or_else(|| StorageError::NotFound {
            entity: "bundle",
            id: bundle_id.to_owned(),
        })?;
        let next = entry.state.trash()?;
        conn.execute(
            "UPDATE bundles SET state = ?1, trashed_at_utc = ?2 WHERE bundle_id = ?3",
            params![bundle_state_to_str(next), trashed_at_utc, bundle_id],
        )
        .map_err(|source| StorageError::sqlite("trash_bundle", source))?;

        entry.state = next;
        entry.trashed_at_utc = Some(trashed_at_utc.to_owned());
        Ok(entry)
    }

    /// Restores a `Trashed` bundle back to `Active`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if the bundle is absent,
    /// [`StorageError::StateTransition`] if it is not `Trashed`, or [`StorageError::Sqlite`].
    pub fn restore_bundle(&self, bundle_id: &str) -> Result<BundleEntry, StorageError> {
        let conn = self.lock_conn();
        let mut entry = fetch_bundle(&conn, bundle_id)?.ok_or_else(|| StorageError::NotFound {
            entity: "bundle",
            id: bundle_id.to_owned(),
        })?;
        let next = entry.state.restore()?;
        conn.execute(
            "UPDATE bundles SET state = ?1, trashed_at_utc = NULL WHERE bundle_id = ?2",
            params![bundle_state_to_str(next), bundle_id],
        )
        .map_err(|source| StorageError::sqlite("restore_bundle", source))?;

        entry.state = next;
        entry.trashed_at_utc = None;
        Ok(entry)
    }

    /// Acquires a reader or job lease on `bundle_id`, returning its unique `lease_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if `bundle_id` does not exist,
    /// [`StorageError::InvalidArgument`] if the bundle is already `Purging`, or
    /// [`StorageError::Sqlite`] on DB error.
    pub fn acquire_lease(
        &self,
        bundle_id: &str,
        holder_kind: LeaseHolderKind,
        holder_pid: u32,
        now_utc: &str,
    ) -> Result<String, StorageError> {
        let conn = self.lock_conn();
        let entry = fetch_bundle(&conn, bundle_id)?.ok_or_else(|| StorageError::NotFound {
            entity: "bundle",
            id: bundle_id.to_owned(),
        })?;
        if entry.state == BundleLifecycleState::Purging {
            return Err(StorageError::InvalidArgument {
                name: "bundle_id",
                reason: format!("bundle `{bundle_id}` is already purging"),
            });
        }

        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let seq = LEASE_SEQ.fetch_add(1, Ordering::Relaxed);
        let lease_id = format!("lease-{holder_pid}-{now_nanos}-{seq}");
        conn.execute(
            "INSERT INTO leases (lease_id, bundle_id, holder_kind, holder_pid, last_heartbeat_utc)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                lease_id,
                bundle_id,
                holder_kind.as_str(),
                i64::from(holder_pid),
                now_utc,
            ],
        )
        .map_err(|source| StorageError::sqlite("acquire_lease", source))?;
        Ok(lease_id)
    }

    /// Renews an active lease's heartbeat timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`] if `lease_id` does not exist, or [`StorageError::Sqlite`].
    pub fn renew_lease(&self, lease_id: &str, now_utc: &str) -> Result<(), StorageError> {
        let conn = self.lock_conn();
        let updated = conn
            .execute(
                "UPDATE leases SET last_heartbeat_utc = ?1 WHERE lease_id = ?2",
                params![now_utc, lease_id],
            )
            .map_err(|source| StorageError::sqlite("renew_lease", source))?;
        if updated == 0 {
            return Err(StorageError::NotFound {
                entity: "lease",
                id: lease_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Releases an active lease by `lease_id`.
    ///
    /// Returns `Ok(true)` if the lease existed and was removed, `Ok(false)` if already gone.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] on database failure.
    pub fn release_lease(&self, lease_id: &str) -> Result<bool, StorageError> {
        let conn = self.lock_conn();
        let deleted = conn
            .execute("DELETE FROM leases WHERE lease_id = ?1", params![lease_id])
            .map_err(|source| StorageError::sqlite("release_lease", source))?;
        Ok(deleted > 0)
    }

    /// Lists all active leases currently held on `bundle_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] or [`StorageError::CorruptData`].
    pub fn active_leases_for_bundle(
        &self,
        bundle_id: &str,
    ) -> Result<Vec<LeaseRecord>, StorageError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare(
                "SELECT lease_id, bundle_id, holder_kind, holder_pid, last_heartbeat_utc
                 FROM leases
                 WHERE bundle_id = ?1
                 ORDER BY lease_id ASC",
            )
            .map_err(|source| StorageError::sqlite("prepare_active_leases", source))?;

        let rows = stmt
            .query_map(params![bundle_id], read_lease_sql_row)
            .map_err(|source| StorageError::sqlite("query_active_leases", source))?;

        let mut leases = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_lease_row", source))?;
            leases.push(decode_lease_row(raw)?);
        }
        Ok(leases)
    }

    /// Physically garbage-collects eligible trashed bundles whose `trashed_at_utc <= now_utc`.
    ///
    /// A bundle is physically removed ONLY if all of the following hold:
    /// 1. `state == BundleLifecycleState::Trashed`
    /// 2. `trashed_at_utc <= now_utc` (if set)
    /// 3. `active_leases == 0`
    /// 4. `active_jobs == 0`
    /// 5. `format_version == CatalogFormatVersion::V2` and `path` is strictly inside
    ///    `managed_bundles_root` (legacy or external paths are NEVER deleted).
    ///
    /// Bundles left in `Purging` (by an earlier failed run or a crash) are retried first.
    /// A failure on one bundle is recorded in [`GcReport::failed`] and the run continues.
    /// Timestamps are compared numerically (ISO-8601 with `Z` or `±HH:MM` offsets and
    /// optional fractional seconds), never as strings.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] if `now_utc` is not an ISO-8601 timestamp, or
    /// [`StorageError::Sqlite`] if the candidate lists cannot be read.
    pub fn gc_trashed_bundles(&self, now_utc: &str) -> Result<GcReport, StorageError> {
        let now_ns =
            parse_utc_timestamp_ns(now_utc).ok_or_else(|| StorageError::InvalidArgument {
                name: "now_utc",
                reason: format!("`{now_utc}` is not an ISO-8601 UTC timestamp"),
            })?;
        let mut report = GcReport::default();

        for bundle in self.list_bundles(Some(BundleLifecycleState::Purging))? {
            if !self.is_purgeable_location(&bundle) {
                report
                    .skipped_unmanaged_or_legacy_bundle_ids
                    .push(bundle.bundle_id);
                continue;
            }
            match self.finish_purge(&bundle) {
                Ok(()) => report.purged_bundle_ids.push(bundle.bundle_id),
                Err(err) => report.failed.push(GcFailure {
                    bundle_id: bundle.bundle_id,
                    error: err.to_string(),
                }),
            }
        }

        for bundle in self.list_bundles(Some(BundleLifecycleState::Trashed))? {
            let in_grace = bundle
                .trashed_at_utc
                .as_deref()
                .is_some_and(|t| parse_utc_timestamp_ns(t).is_none_or(|t_ns| t_ns > now_ns));
            if in_grace {
                report.skipped_in_grace_bundle_ids.push(bundle.bundle_id);
                continue;
            }

            if !self.is_purgeable_location(&bundle) {
                report
                    .skipped_unmanaged_or_legacy_bundle_ids
                    .push(bundle.bundle_id);
                continue;
            }

            let outcome = self.try_mark_purging(&bundle).and_then(|gate| match gate {
                PurgeGate::Marked => self.finish_purge(&bundle).map(|()| gate),
                other => Ok(other),
            });
            match outcome {
                Ok(PurgeGate::Marked) => report.purged_bundle_ids.push(bundle.bundle_id),
                Ok(PurgeGate::Leased) => report.skipped_leased_bundle_ids.push(bundle.bundle_id),
                Ok(PurgeGate::ActiveJobs) => {
                    report.skipped_active_job_bundle_ids.push(bundle.bundle_id);
                }
                Ok(PurgeGate::Gone) => {}
                Err(err) => report.failed.push(GcFailure {
                    bundle_id: bundle.bundle_id,
                    error: err.to_string(),
                }),
            }
        }

        Ok(report)
    }

    fn is_purgeable_location(&self, bundle: &BundleEntry) -> bool {
        bundle.format_version == CatalogFormatVersion::V2
            && is_strictly_inside_managed_root(&bundle.path, &self.managed_bundles_root)
    }

    /// Atomically re-checks a `Trashed` bundle (state, leases, active jobs) and marks it
    /// `Purging` under the connection mutex.
    fn try_mark_purging(&self, bundle: &BundleEntry) -> Result<PurgeGate, StorageError> {
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction()
            .map_err(|source| StorageError::sqlite("begin_gc_tx", source))?;

        let Some(current) = fetch_bundle(&tx, &bundle.bundle_id)? else {
            return Ok(PurgeGate::Gone);
        };
        if current.state != BundleLifecycleState::Trashed {
            return Ok(PurgeGate::Gone);
        }
        let active_leases = count_active_leases(&tx, &bundle.bundle_id)?;
        if active_leases > 0 {
            return Ok(PurgeGate::Leased);
        }
        let active_jobs = count_active_jobs_for_path(&tx, &bundle.path)?;
        if active_jobs > 0 {
            return Ok(PurgeGate::ActiveJobs);
        }

        let purging = current.state.begin_purge(active_leases, active_jobs)?;
        tx.execute(
            "UPDATE bundles SET state = ?1 WHERE bundle_id = ?2",
            params![bundle_state_to_str(purging), bundle.bundle_id],
        )
        .map_err(|source| StorageError::sqlite("mark_bundle_purging", source))?;
        tx.commit()
            .map_err(|source| StorageError::sqlite("commit_gc_mark", source))?;
        Ok(PurgeGate::Marked)
    }

    /// Physically removes a `Purging` bundle directory, then deletes its catalog rows.
    fn finish_purge(&self, bundle: &BundleEntry) -> Result<(), StorageError> {
        remove_dir_if_exists(&bundle.path)?;
        let mut conn = self.lock_conn();
        let tx = conn
            .transaction()
            .map_err(|source| StorageError::sqlite("begin_gc_delete_tx", source))?;
        tx.execute(
            "DELETE FROM edit_sessions WHERE bundle_id = ?1",
            params![bundle.bundle_id],
        )
        .map_err(|source| StorageError::sqlite("delete_bundle_edit_sessions", source))?;
        tx.execute(
            "DELETE FROM bundles WHERE bundle_id = ?1",
            params![bundle.bundle_id],
        )
        .map_err(|source| StorageError::sqlite("delete_purged_bundle", source))?;
        tx.commit()
            .map_err(|source| StorageError::sqlite("commit_gc_delete_tx", source))
    }

    /// Reconciles durable pin journals (`save_jobs/<job>.json`) with the catalog's
    /// `save_jobs` rows so pins can never leak forever (R2 S13). Call it during startup
    /// recovery, after [`Self::recover_on_startup`] (see [`Self::recover_on_startup_with_pins`]):
    ///
    /// - a journal whose job is `Completed`, `Discarded`, or unknown to the catalog is
    ///   unpinned (crash between `complete_job`/`discard_job` and `unpin_job`);
    /// - an active or retryable job (`Pinned`/`Draining`/`Materializing`/`FailedRetryable`)
    ///   whose journal is missing gets it re-created from the catalog's pinned chunk IDs, so
    ///   the ring cannot evict chunks a retry still needs. IDs of chunks that no longer exist
    ///   are harmless in a journal (they protect nothing); the retry then either finds the
    ///   already committed bundle or fails retryably.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`], [`StorageError::Io`] or [`StorageError::Json`] if the
    /// catalog or a journal cannot be read or written.
    pub fn reconcile_pins(
        &self,
        journal: &PinJournal,
        now_utc: &str,
    ) -> Result<PinReconcileReport, StorageError> {
        let mut report = PinReconcileReport::default();
        let jobs: std::collections::BTreeMap<String, CatalogSaveJob> = self
            .list_save_jobs()?
            .into_iter()
            .map(|j| (j.job.job_id.clone(), j))
            .collect();
        let journals = journal.list_all()?;
        let journaled: std::collections::BTreeSet<String> =
            journals.iter().map(|r| r.job_id.clone()).collect();

        for record in &journals {
            let releasable = jobs
                .get(&record.job_id)
                .is_none_or(|job| job.job.state.is_terminal());
            if releasable && journal.unpin(&record.job_id)? {
                report.released_job_ids.push(record.job_id.clone());
            }
        }

        for (job_id, job) in &jobs {
            if !ACTIVE_JOB_STATES.contains(&job.job.state)
                || journaled.contains(job_id)
                || job.job.pinned_chunk_ids.is_empty()
            {
                continue;
            }
            journal.pin(&PinJournalRecord {
                job_id: job_id.clone(),
                destination_path: PathBuf::from(&job.job.destination_path),
                pinned_chunk_ids: job.job.pinned_chunk_ids.clone(),
                created_at_utc: now_utc.to_owned(),
            })?;
            report.repinned_job_ids.push(job_id.clone());
        }
        Ok(report)
    }

    /// [`Self::recover_on_startup`] followed by [`Self::reconcile_pins`].
    ///
    /// # Errors
    ///
    /// Returns any error from either step.
    pub fn recover_on_startup_with_pins(
        &self,
        now_utc: &str,
        journal: &PinJournal,
    ) -> Result<(RecoveryReport, PinReconcileReport), StorageError> {
        let recovery = self.recover_on_startup(now_utc)?;
        let pins = self.reconcile_pins(journal, now_utc)?;
        Ok((recovery, pins))
    }

    /// Creates a new save job in `Pinned` state with its Phase-1 pinned chunk IDs.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] if `job_id` or `destination_path`
    /// is empty, or [`StorageError::Sqlite`] / [`StorageError::Json`] on failure.
    pub fn create_pinned_job(
        &self,
        job_id: &str,
        destination_path: impl AsRef<Path>,
        pinned_chunk_ids: &[String],
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        if job_id.trim().is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "job_id",
                reason: "job_id must not be empty".to_owned(),
            });
        }
        let dest_str = destination_path.as_ref().to_string_lossy().into_owned();
        if dest_str.trim().is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "destination_path",
                reason: "destination_path must not be empty".to_owned(),
            });
        }
        let job = SaveJob::new(job_id, dest_str.clone(), pinned_chunk_ids.to_vec());
        let chunks_json = serde_json::to_string(&job.pinned_chunk_ids)
            .map_err(|source| StorageError::json("serialize_pinned_chunks", source))?;

        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO save_jobs (
                job_id, state, destination_path, pinned_chunks_json,
                error_message, created_at_utc, updated_at_utc
            ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
            params![
                job.job_id,
                save_job_state_to_str(job.state),
                dest_str,
                chunks_json,
                now_utc,
                now_utc,
            ],
        )
        .map_err(|source| StorageError::sqlite("create_pinned_job", source))?;

        Ok(CatalogSaveJob {
            job,
            created_at_utc: now_utc.to_owned(),
            updated_at_utc: now_utc.to_owned(),
        })
    }

    /// Transitions a save job from `Pinned` to `Draining`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn mark_draining(
        &self,
        job_id: &str,
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        self.apply_save_job_event(job_id, SaveJobEvent::BeginDrain, now_utc)
    }

    /// Transitions a save job from `Draining` to `Materializing`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn mark_materializing(
        &self,
        job_id: &str,
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        self.apply_save_job_event(job_id, SaveJobEvent::BeginMaterialize, now_utc)
    }

    /// Transitions a `Pinned`, `Draining`, or `Materializing` save job into
    /// `FailedRetryable`, preserving `pinned_chunk_ids` intact.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn mark_failed_retryable(
        &self,
        job_id: &str,
        error_message: &str,
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        self.apply_save_job_event(
            job_id,
            SaveJobEvent::FailRetryable {
                error_message: error_message.to_owned(),
            },
            now_utc,
        )
    }

    /// Retries a `FailedRetryable` save job (`FailedRetryable -> Materializing`)
    /// to the same destination with pins preserved.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn retry_job(&self, job_id: &str, now_utc: &str) -> Result<CatalogSaveJob, StorageError> {
        self.apply_save_job_event(job_id, SaveJobEvent::Retry, now_utc)
    }

    /// Retargets a `FailedRetryable` save job to `new_dest` (`FailedRetryable -> Materializing`)
    /// with pins preserved.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn retarget_job(
        &self,
        job_id: &str,
        new_dest: impl AsRef<Path>,
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        let new_destination_path = new_dest.as_ref().to_string_lossy().into_owned();
        self.apply_save_job_event(
            job_id,
            SaveJobEvent::Retarget {
                new_destination_path,
            },
            now_utc,
        )
    }

    /// Completes a `Materializing` save job (`Materializing -> Completed`) and releases its pins.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn complete_job(
        &self,
        job_id: &str,
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        self.apply_save_job_event(job_id, SaveJobEvent::Complete, now_utc)
    }

    /// Explicitly discards a `Pinned`, `Draining`, or `FailedRetryable` save job and releases its pins.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::NotFound`], [`StorageError::StateTransition`], or [`StorageError::Sqlite`].
    pub fn discard_job(&self, job_id: &str, now_utc: &str) -> Result<CatalogSaveJob, StorageError> {
        self.apply_save_job_event(job_id, SaveJobEvent::Discard, now_utc)
    }

    /// Fetches a single save job by `job_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] or [`StorageError::CorruptData`].
    pub fn save_job(&self, job_id: &str) -> Result<Option<CatalogSaveJob>, StorageError> {
        let conn = self.lock_conn();
        fetch_save_job(&conn, job_id)
    }

    /// Lists all save jobs ordered by creation timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] or [`StorageError::CorruptData`].
    pub fn list_save_jobs(&self) -> Result<Vec<CatalogSaveJob>, StorageError> {
        let conn = self.lock_conn();
        let mut stmt = conn
            .prepare(
                "SELECT job_id, state, destination_path, pinned_chunks_json,
                        error_message, created_at_utc, updated_at_utc
                 FROM save_jobs
                 ORDER BY created_at_utc ASC, job_id ASC",
            )
            .map_err(|source| StorageError::sqlite("prepare_list_save_jobs", source))?;

        let rows = stmt
            .query_map([], read_save_job_sql_row)
            .map_err(|source| StorageError::sqlite("list_save_jobs", source))?;

        let mut jobs = Vec::new();
        for row in rows {
            let raw = row.map_err(|source| StorageError::sqlite("read_save_job_row", source))?;
            jobs.push(decode_save_job_row(raw)?);
        }
        Ok(jobs)
    }

    /// Runs startup crash recovery:
    /// 1. Cleans orphan `.staging-*` directories in `managed_bundles_root`.
    /// 2. Reaps leases whose `arb_os::fs::is_pid_alive(holder_pid) == false`.
    /// 3. Restores interrupted `pinned` / `draining` / `materializing` save jobs
    ///    to `failed_retryable` with their pinned chunk IDs intact.
    /// 4. Completes cleanup of any managed non-legacy bundles left in `purging`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] or [`StorageError::Sqlite`] on recovery failures.
    pub fn recover_on_startup(&self, now_utc: &str) -> Result<RecoveryReport, StorageError> {
        let removed_staging_dirs = self.cleanup_orphan_staging_dirs()?;
        let recovered = self.recover_catalog_rows(now_utc)?;
        let completed_purging_bundle_ids = self.finish_purging_bundles(recovered.purging_bundles);

        Ok(RecoveryReport {
            removed_staging_dirs,
            reaped_lease_ids: recovered.reaped_lease_ids,
            recovered_job_ids: recovered.recovered_job_ids,
            completed_purging_bundle_ids,
        })
    }

    fn cleanup_orphan_staging_dirs(&self) -> Result<Vec<PathBuf>, StorageError> {
        if !self.managed_bundles_root.is_dir() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.managed_bundles_root).map_err(|source| {
            StorageError::io(
                "scan_managed_bundles_root",
                &self.managed_bundles_root,
                source,
            )
        })?;
        let mut orphan_paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| {
                StorageError::io(
                    "iter_managed_bundles_root",
                    &self.managed_bundles_root,
                    source,
                )
            })?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with(".staging-") && path.is_dir() {
                orphan_paths.push(path);
            }
        }
        orphan_paths.sort();
        for path in &orphan_paths {
            remove_dir_if_exists(path)?;
        }
        Ok(orphan_paths)
    }

    fn recover_catalog_rows(&self, now_utc: &str) -> Result<CatalogRowRecovery, StorageError> {
        let mut reaped_lease_ids = Vec::new();
        let mut recovered_job_ids = Vec::new();
        let mut purging_bundles = Vec::new();

        let mut conn = self.lock_conn();
        let tx = conn
            .transaction()
            .map_err(|source| StorageError::sqlite("begin_recovery_tx", source))?;

        for lease in fetch_all_leases(&tx)? {
            if !is_pid_alive(lease.holder_pid) {
                tx.execute(
                    "DELETE FROM leases WHERE lease_id = ?1",
                    params![lease.lease_id],
                )
                .map_err(|source| StorageError::sqlite("reap_dead_lease", source))?;
                reaped_lease_ids.push(lease.lease_id);
            }
        }

        for mut record in fetch_interrupted_save_jobs(&tx)? {
            record.job.apply(SaveJobEvent::FailRetryable {
                error_message: "interrupted before completion; recovered on startup".to_owned(),
            })?;
            let chunks_json = serde_json::to_string(&record.job.pinned_chunk_ids)
                .map_err(|source| StorageError::json("serialize_recovered_pins", source))?;
            tx.execute(
                "UPDATE save_jobs
                 SET state = ?1, pinned_chunks_json = ?2, error_message = ?3, updated_at_utc = ?4
                 WHERE job_id = ?5",
                params![
                    save_job_state_to_str(record.job.state),
                    chunks_json,
                    record.job.last_error,
                    now_utc,
                    record.job.job_id,
                ],
            )
            .map_err(|source| StorageError::sqlite("recover_save_job", source))?;
            recovered_job_ids.push(record.job.job_id);
        }

        {
            let mut stmt = tx
                .prepare(
                    "SELECT bundle_id, path, format_version, state, trashed_at_utc,
                            parent_bundle_id, created_at_utc
                     FROM bundles
                     WHERE state = 'purging'",
                )
                .map_err(|source| StorageError::sqlite("prepare_purging_bundles", source))?;
            let rows = stmt
                .query_map([], read_bundle_sql_row)
                .map_err(|source| StorageError::sqlite("query_purging_bundles", source))?;
            for row in rows {
                let raw = row.map_err(|source| StorageError::sqlite("read_purging_row", source))?;
                purging_bundles.push(decode_bundle_row(raw)?);
            }
        }

        tx.commit()
            .map_err(|source| StorageError::sqlite("commit_recovery_tx", source))?;
        Ok(CatalogRowRecovery {
            reaped_lease_ids,
            recovered_job_ids,
            purging_bundles,
        })
    }

    fn finish_purging_bundles(&self, purging_bundles: Vec<BundleEntry>) -> Vec<String> {
        let mut completed = Vec::new();
        for bundle in purging_bundles {
            // A bundle that cannot be removed right now (e.g. locked by another process)
            // stays `Purging`; `gc_trashed_bundles` retries it and reports the failure.
            if self.is_purgeable_location(&bundle) && self.finish_purge(&bundle).is_ok() {
                completed.push(bundle.bundle_id);
            }
        }
        completed
    }

    /// Inserts or updates a non-destructive [`EditSession`] outside the immutable bundle.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidArgument`] if `session_id` is empty or
    /// `in_frame_index > out_frame_index`, or [`StorageError::Sqlite`] on DB error
    /// (including foreign-key failure if `bundle_id` is unknown).
    pub fn upsert_edit_session(&self, session: &EditSession) -> Result<(), StorageError> {
        if session.session_id.trim().is_empty() {
            return Err(StorageError::InvalidArgument {
                name: "session_id",
                reason: "session_id must not be empty".to_owned(),
            });
        }
        if let (Some(in_f), Some(out_f)) = (session.in_frame_index, session.out_frame_index)
            && in_f > out_f
        {
            return Err(StorageError::InvalidArgument {
                name: "in_frame_index",
                reason: format!("in_frame_index ({in_f}) must be <= out_frame_index ({out_f})"),
            });
        }

        let in_frame = opt_u64_to_i64(session.in_frame_index, "in_frame_index")?;
        let out_frame = opt_u64_to_i64(session.out_frame_index, "out_frame_index")?;
        let playhead = opt_u64_to_i64(session.playhead_frame_index, "playhead_frame_index")?;

        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO edit_sessions (
                session_id, bundle_id, in_frame_index, out_frame_index,
                playhead_frame_index, av_offset_override_ms, updated_at_utc
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(session_id) DO UPDATE SET
                bundle_id = excluded.bundle_id,
                in_frame_index = excluded.in_frame_index,
                out_frame_index = excluded.out_frame_index,
                playhead_frame_index = excluded.playhead_frame_index,
                av_offset_override_ms = excluded.av_offset_override_ms,
                updated_at_utc = excluded.updated_at_utc",
            params![
                session.session_id,
                session.bundle_id,
                in_frame,
                out_frame,
                playhead,
                session.av_offset_override_ms,
                session.updated_at_utc,
            ],
        )
        .map_err(|source| StorageError::sqlite("upsert_edit_session", source))?;
        Ok(())
    }

    /// Loads an [`EditSession`] by `session_id`, if present.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] or [`StorageError::CorruptData`].
    pub fn get_edit_session(&self, session_id: &str) -> Result<Option<EditSession>, StorageError> {
        let conn = self.lock_conn();
        let raw = conn
            .query_row(
                "SELECT session_id, bundle_id, in_frame_index, out_frame_index,
                        playhead_frame_index, av_offset_override_ms, updated_at_utc
                 FROM edit_sessions
                 WHERE session_id = ?1",
                params![session_id],
                |row| {
                    Ok(RawEditSessionRow {
                        session_id: row.get(0)?,
                        bundle_id: row.get(1)?,
                        in_frame_index: row.get(2)?,
                        out_frame_index: row.get(3)?,
                        playhead_frame_index: row.get(4)?,
                        av_offset_override_ms: row.get(5)?,
                        updated_at_utc: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|source| StorageError::sqlite("get_edit_session", source))?;

        raw.map(decode_edit_session_row).transpose()
    }

    fn apply_save_job_event(
        &self,
        job_id: &str,
        event: SaveJobEvent,
        now_utc: &str,
    ) -> Result<CatalogSaveJob, StorageError> {
        let conn = self.lock_conn();
        let mut record = fetch_save_job(&conn, job_id)?.ok_or_else(|| StorageError::NotFound {
            entity: "save_job",
            id: job_id.to_owned(),
        })?;
        record.job.apply(event)?;
        let chunks_json = serde_json::to_string(&record.job.pinned_chunk_ids)
            .map_err(|source| StorageError::json("serialize_pinned_chunks", source))?;
        conn.execute(
            "UPDATE save_jobs
             SET state = ?1, destination_path = ?2, pinned_chunks_json = ?3,
                 error_message = ?4, updated_at_utc = ?5
             WHERE job_id = ?6",
            params![
                save_job_state_to_str(record.job.state),
                record.job.destination_path,
                chunks_json,
                record.job.last_error,
                now_utc,
                job_id,
            ],
        )
        .map_err(|source| StorageError::sqlite("update_save_job", source))?;
        now_utc.clone_into(&mut record.updated_at_utc);
        Ok(record)
    }

    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("catalog SQLite mutex poisoned")
    }
}

// ---------------------------------------------------------------------------
// Internal SQL & Path Safety Helpers
// ---------------------------------------------------------------------------

/// Outcome of the transactional eligibility check in `try_mark_purging`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PurgeGate {
    Marked,
    Leased,
    ActiveJobs,
    Gone,
}

/// Parses an ISO-8601 timestamp (`YYYY-MM-DD[T ]HH:MM:SS[.fraction](Z|±HH:MM|±HHMM)`; no
/// offset means UTC) into Unix nanoseconds, so catalog timestamps compare numerically.
pub fn parse_utc_timestamp_ns(value: &str) -> Option<i128> {
    let b = value.trim().as_bytes();
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = b.get(range)?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    };
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if second > 60 {
        return None;
    }
    let mut pos = 19;
    let mut frac_ns: i128 = 0;
    if b.get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while b.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        let digits = &b[start..pos];
        if digits.is_empty() {
            return None;
        }
        for (i, d) in digits.iter().take(9).enumerate() {
            let place = 8 - u32::try_from(i).ok()?;
            frac_ns += i128::from(d - b'0') * 10i128.pow(place);
        }
    }
    let offset_secs: i64 = match b.get(pos) {
        None => 0,
        Some(b'Z' | b'z') if pos + 1 == b.len() => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let rest = &b[pos + 1..];
            let (oh, om) = match rest.len() {
                5 if rest[2] == b':' => (num(pos + 1..pos + 3)?, num(pos + 4..pos + 6)?),
                4 => (num(pos + 1..pos + 3)?, num(pos + 3..pos + 5)?),
                _ => return None,
            };
            let secs = oh * 3_600 + om * 60;
            if sign == b'+' { secs } else { -secs }
        }
        Some(_) => return None,
    };
    // Days from civil (proleptic Gregorian), Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs;
    Some(i128::from(secs) * 1_000_000_000 + frac_ns)
}

struct CatalogRowRecovery {
    reaped_lease_ids: Vec<String>,
    recovered_job_ids: Vec<String>,
    purging_bundles: Vec<BundleEntry>,
}

struct RawBundleRow {
    bundle_id: String,
    path: String,
    format_version: String,
    state: String,
    trashed_at_utc: Option<String>,
    parent_bundle_id: Option<String>,
    created_at_utc: String,
}

fn read_bundle_sql_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawBundleRow> {
    Ok(RawBundleRow {
        bundle_id: row.get(0)?,
        path: row.get(1)?,
        format_version: row.get(2)?,
        state: row.get(3)?,
        trashed_at_utc: row.get(4)?,
        parent_bundle_id: row.get(5)?,
        created_at_utc: row.get(6)?,
    })
}

fn decode_bundle_row(raw: RawBundleRow) -> Result<BundleEntry, StorageError> {
    Ok(BundleEntry {
        bundle_id: raw.bundle_id,
        path: PathBuf::from(raw.path),
        format_version: CatalogFormatVersion::from_db(&raw.format_version)?,
        state: parse_bundle_state(&raw.state)?,
        trashed_at_utc: raw.trashed_at_utc,
        parent_bundle_id: raw.parent_bundle_id,
        created_at_utc: raw.created_at_utc,
    })
}

fn fetch_bundle(conn: &Connection, bundle_id: &str) -> Result<Option<BundleEntry>, StorageError> {
    let raw = conn
        .query_row(
            "SELECT bundle_id, path, format_version, state, trashed_at_utc,
                    parent_bundle_id, created_at_utc
             FROM bundles
             WHERE bundle_id = ?1",
            params![bundle_id],
            read_bundle_sql_row,
        )
        .optional()
        .map_err(|source| StorageError::sqlite("fetch_bundle", source))?;
    raw.map(decode_bundle_row).transpose()
}

struct RawLeaseRow {
    lease_id: String,
    bundle_id: String,
    holder_kind: String,
    holder_pid: i64,
    last_heartbeat_utc: String,
}

fn read_lease_sql_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawLeaseRow> {
    Ok(RawLeaseRow {
        lease_id: row.get(0)?,
        bundle_id: row.get(1)?,
        holder_kind: row.get(2)?,
        holder_pid: row.get(3)?,
        last_heartbeat_utc: row.get(4)?,
    })
}

fn decode_lease_row(raw: RawLeaseRow) -> Result<LeaseRecord, StorageError> {
    let holder_pid = u32::try_from(raw.holder_pid).map_err(|_| StorageError::CorruptData {
        reason: format!("invalid holder_pid {} in leases table", raw.holder_pid),
    })?;
    Ok(LeaseRecord {
        lease_id: raw.lease_id,
        bundle_id: raw.bundle_id,
        holder_kind: LeaseHolderKind::from_db(&raw.holder_kind)?,
        holder_pid,
        last_heartbeat_utc: raw.last_heartbeat_utc,
    })
}

fn fetch_all_leases(conn: &Connection) -> Result<Vec<LeaseRecord>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT lease_id, bundle_id, holder_kind, holder_pid, last_heartbeat_utc
             FROM leases
             ORDER BY lease_id ASC",
        )
        .map_err(|source| StorageError::sqlite("prepare_all_leases", source))?;
    let rows = stmt
        .query_map([], read_lease_sql_row)
        .map_err(|source| StorageError::sqlite("query_all_leases", source))?;
    let mut leases = Vec::new();
    for row in rows {
        let raw = row.map_err(|source| StorageError::sqlite("read_lease_row", source))?;
        leases.push(decode_lease_row(raw)?);
    }
    Ok(leases)
}

struct RawSaveJobRow {
    job_id: String,
    state: String,
    destination_path: String,
    pinned_chunks_json: String,
    error_message: Option<String>,
    created_at_utc: String,
    updated_at_utc: String,
}

fn read_save_job_sql_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSaveJobRow> {
    Ok(RawSaveJobRow {
        job_id: row.get(0)?,
        state: row.get(1)?,
        destination_path: row.get(2)?,
        pinned_chunks_json: row.get(3)?,
        error_message: row.get(4)?,
        created_at_utc: row.get(5)?,
        updated_at_utc: row.get(6)?,
    })
}

fn decode_save_job_row(raw: RawSaveJobRow) -> Result<CatalogSaveJob, StorageError> {
    let state = parse_save_job_state(&raw.state)?;
    let pinned_chunk_ids: Vec<String> = serde_json::from_str(&raw.pinned_chunks_json)
        .map_err(|source| StorageError::json("deserialize_pinned_chunks", source))?;
    Ok(CatalogSaveJob {
        job: SaveJob {
            job_id: raw.job_id,
            state,
            destination_path: raw.destination_path,
            pinned_chunk_ids,
            last_error: raw.error_message,
        },
        created_at_utc: raw.created_at_utc,
        updated_at_utc: raw.updated_at_utc,
    })
}

fn fetch_save_job(conn: &Connection, job_id: &str) -> Result<Option<CatalogSaveJob>, StorageError> {
    let raw = conn
        .query_row(
            "SELECT job_id, state, destination_path, pinned_chunks_json,
                    error_message, created_at_utc, updated_at_utc
             FROM save_jobs
             WHERE job_id = ?1",
            params![job_id],
            read_save_job_sql_row,
        )
        .optional()
        .map_err(|source| StorageError::sqlite("fetch_save_job", source))?;
    raw.map(decode_save_job_row).transpose()
}

fn fetch_interrupted_save_jobs(conn: &Connection) -> Result<Vec<CatalogSaveJob>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT job_id, state, destination_path, pinned_chunks_json,
                    error_message, created_at_utc, updated_at_utc
             FROM save_jobs
             WHERE state IN ('pinned', 'draining', 'materializing')
             ORDER BY created_at_utc ASC, job_id ASC",
        )
        .map_err(|source| StorageError::sqlite("prepare_interrupted_save_jobs", source))?;
    let rows = stmt
        .query_map([], read_save_job_sql_row)
        .map_err(|source| StorageError::sqlite("query_interrupted_save_jobs", source))?;
    let mut jobs = Vec::new();
    for row in rows {
        let raw = row.map_err(|source| StorageError::sqlite("read_save_job_row", source))?;
        jobs.push(decode_save_job_row(raw)?);
    }
    Ok(jobs)
}

struct RawEditSessionRow {
    session_id: String,
    bundle_id: String,
    in_frame_index: Option<i64>,
    out_frame_index: Option<i64>,
    playhead_frame_index: Option<i64>,
    av_offset_override_ms: Option<i64>,
    updated_at_utc: String,
}

fn decode_edit_session_row(raw: RawEditSessionRow) -> Result<EditSession, StorageError> {
    Ok(EditSession {
        session_id: raw.session_id,
        bundle_id: raw.bundle_id,
        in_frame_index: opt_i64_to_u64(raw.in_frame_index, "in_frame_index")?,
        out_frame_index: opt_i64_to_u64(raw.out_frame_index, "out_frame_index")?,
        playhead_frame_index: opt_i64_to_u64(raw.playhead_frame_index, "playhead_frame_index")?,
        av_offset_override_ms: raw.av_offset_override_ms,
        updated_at_utc: raw.updated_at_utc,
    })
}

fn count_active_leases(conn: &Connection, bundle_id: &str) -> Result<usize, StorageError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM leases WHERE bundle_id = ?1",
            params![bundle_id],
            |row| row.get(0),
        )
        .map_err(|source| StorageError::sqlite("count_active_leases", source))?;
    usize::try_from(count).map_err(|_| StorageError::CorruptData {
        reason: format!("negative lease count {count}"),
    })
}

fn count_active_jobs_for_path(
    conn: &Connection,
    bundle_path: &Path,
) -> Result<usize, StorageError> {
    let target_norm = canonical_or_lexical(bundle_path);
    let mut stmt = conn
        .prepare(
            "SELECT destination_path
             FROM save_jobs
             WHERE state IN ('pinned', 'draining', 'materializing', 'failed_retryable')",
        )
        .map_err(|source| StorageError::sqlite("prepare_active_jobs_for_path", source))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| StorageError::sqlite("query_active_jobs_for_path", source))?;

    let mut matches: usize = 0;
    for row in rows {
        let raw_dest =
            row.map_err(|source| StorageError::sqlite("read_active_job_dest", source))?;
        let dest_norm = canonical_or_lexical(Path::new(&raw_dest));
        if dest_norm == target_norm {
            matches = matches.saturating_add(1);
        }
    }
    Ok(matches)
}

fn is_strictly_inside_managed_root(candidate: &Path, managed_root: &Path) -> bool {
    // Reject symlinks pointing outside the managed directory.
    if let Ok(meta) = fs::symlink_metadata(candidate)
        && meta.file_type().is_symlink()
    {
        return false;
    }
    let resolved_root = canonical_or_lexical(managed_root);
    let resolved_candidate = canonical_or_lexical(candidate);
    resolved_candidate != resolved_root && resolved_candidate.starts_with(&resolved_root)
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && let Ok(canon_parent) = fs::canonicalize(parent)
    {
        return canon_parent.join(name);
    }
    normalize_lexical(path)
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn remove_dir_if_exists(path: &Path) -> Result<(), StorageError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StorageError::io("remove_managed_dir", path, source)),
    }
}

const fn bundle_state_to_str(state: BundleLifecycleState) -> &'static str {
    match state {
        BundleLifecycleState::Active => "active",
        BundleLifecycleState::Trashed => "trashed",
        _ => "purging",
    }
}

fn parse_bundle_state(value: &str) -> Result<BundleLifecycleState, StorageError> {
    match value {
        "active" => Ok(BundleLifecycleState::Active),
        "trashed" => Ok(BundleLifecycleState::Trashed),
        "purging" => Ok(BundleLifecycleState::Purging),
        other => Err(StorageError::CorruptData {
            reason: format!("unknown bundles.state `{other}`"),
        }),
    }
}

const fn save_job_state_to_str(state: SaveJobState) -> &'static str {
    match state {
        SaveJobState::Pinned => "pinned",
        SaveJobState::Draining => "draining",
        SaveJobState::Materializing => "materializing",
        SaveJobState::FailedRetryable => "failed_retryable",
        SaveJobState::Completed => "completed",
        _ => "discarded",
    }
}

fn parse_save_job_state(value: &str) -> Result<SaveJobState, StorageError> {
    match value {
        "pinned" => Ok(SaveJobState::Pinned),
        "draining" => Ok(SaveJobState::Draining),
        "materializing" => Ok(SaveJobState::Materializing),
        "failed_retryable" => Ok(SaveJobState::FailedRetryable),
        "completed" => Ok(SaveJobState::Completed),
        "discarded" => Ok(SaveJobState::Discarded),
        other => Err(StorageError::CorruptData {
            reason: format!("unknown save_jobs.state `{other}`"),
        }),
    }
}

fn opt_u64_to_i64(value: Option<u64>, field: &'static str) -> Result<Option<i64>, StorageError> {
    value
        .map(|v| {
            i64::try_from(v).map_err(|_| StorageError::InvalidArgument {
                name: field,
                reason: format!("{field} value {v} exceeds SQLite i64::MAX"),
            })
        })
        .transpose()
}

fn opt_i64_to_u64(value: Option<i64>, field: &'static str) -> Result<Option<u64>, StorageError> {
    value
        .map(|v| {
            u64::try_from(v).map_err(|_| StorageError::CorruptData {
                reason: format!("negative {field} value {v} in catalog"),
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use arb_core::state::{BundleLifecycleState, SaveJobState};
    use tempfile::TempDir;

    use super::{Catalog, CatalogFormatVersion, EditSession, LeaseHolderKind, NewBundleEntry};

    #[test]
    fn edit_sessions_and_save_job_lifecycle_persist_in_wal_catalog() {
        let tmp = TempDir::new().expect("temp dir must be created");
        let managed_root = tmp.path().join("bundles");
        let db_path = tmp.path().join("catalog.sqlite");
        let catalog = Catalog::open(&db_path, &managed_root).expect("catalog open must succeed");

        let bundle_dir = managed_root.join("b-1");
        std::fs::create_dir_all(&bundle_dir).expect("bundle dir must be created");
        catalog
            .register_bundle(&NewBundleEntry {
                bundle_id: "b-1".to_owned(),
                path: bundle_dir,
                format_version: CatalogFormatVersion::V2,
                parent_bundle_id: None,
                created_at_utc: "2026-11-22T10:00:00Z".to_owned(),
            })
            .expect("register_bundle must succeed");

        let session = EditSession {
            session_id: "ed-1".to_owned(),
            bundle_id: "b-1".to_owned(),
            in_frame_index: Some(10),
            out_frame_index: Some(90),
            playhead_frame_index: Some(42),
            av_offset_override_ms: Some(-85),
            updated_at_utc: "2026-11-22T10:05:00Z".to_owned(),
        };
        catalog
            .upsert_edit_session(&session)
            .expect("upsert_edit_session must succeed");
        let loaded = catalog
            .get_edit_session("ed-1")
            .expect("get_edit_session must succeed");
        assert_eq!(
            loaded,
            Some(session),
            "edit session must round-trip cleanly"
        );

        let lease_id = catalog
            .acquire_lease(
                "b-1",
                LeaseHolderKind::IdeViewer,
                std::process::id(),
                "2026-11-22T10:06:00Z",
            )
            .expect("acquire_lease must succeed");
        catalog
            .renew_lease(&lease_id, "2026-11-22T10:07:00Z")
            .expect("renew_lease must succeed");
        assert!(
            catalog
                .release_lease(&lease_id)
                .expect("release_lease must succeed"),
            "released lease must report true"
        );

        let created = catalog
            .create_pinned_job(
                "job-1",
                managed_root.join("b-2"),
                &["c1".to_owned(), "c2".to_owned()],
                "2026-11-22T10:08:00Z",
            )
            .expect("create_pinned_job must succeed");
        assert_eq!(
            created.job.state,
            SaveJobState::Pinned,
            "new job must start in Pinned state"
        );
        let discarded = catalog
            .discard_job("job-1", "2026-11-22T10:09:00Z")
            .expect("discard_job must succeed");
        assert_eq!(
            discarded.job.state,
            SaveJobState::Discarded,
            "discarded job must enter Discarded state"
        );
        assert!(
            !discarded.job.has_pinned_chunks(),
            "discarded job must release pinned chunk IDs"
        );

        let list = catalog
            .list_bundles(Some(BundleLifecycleState::Active))
            .expect("list_bundles must succeed");
        assert_eq!(list.len(), 1, "one active bundle expected");
    }
}
