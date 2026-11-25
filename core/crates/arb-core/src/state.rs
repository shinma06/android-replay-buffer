//! Lifecycle state machines for save jobs, WORM bundles, and ring disk budgets.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default live ring buffer size budget (`1.5 GiB`).
pub const DEFAULT_MAX_RING_BYTES: u64 = 1_536 * 1024 * 1024;

/// Default live ring buffer retention duration target (`10 minutes`).
pub const DEFAULT_MAX_RING_DURATION_NS: u64 = 600_000_000_000;

/// Default minimum host free disk space floor (`3 GiB`).
pub const DEFAULT_MIN_FREE_DISK_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Errors returned by illegal state transitions or blocked lifecycle operations.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StateTransitionError {
    /// Save job event was not valid from the current state.
    #[error("invalid save job transition from {from:?} via {event}")]
    InvalidSaveJobTransition {
        /// Current state of the save job.
        from: SaveJobState,
        /// Name of the rejected event.
        event: &'static str,
    },
    /// Retarget destination path was empty.
    #[error("new save destination path must not be empty")]
    EmptyDestinationPath,
    /// Bundle lifecycle transition was invalid for the current state.
    #[error("invalid bundle lifecycle transition from {from:?} to {to:?}")]
    InvalidBundleTransition {
        /// Current bundle state.
        from: BundleLifecycleState,
        /// Attempted target bundle state.
        to: BundleLifecycleState,
    },
    /// Physical purge was blocked because the bundle is not trashed or still referenced.
    #[error(
        "bundle purge blocked in state {state:?} (active_leases={active_leases}, active_jobs={active_jobs})"
    )]
    PurgeBlocked {
        /// Current bundle state.
        state: BundleLifecycleState,
        /// Active reader/export leases still referencing this bundle.
        active_leases: usize,
        /// Active jobs still referencing this bundle.
        active_jobs: usize,
    },
}

/// Reason that initiated a save job.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveTriggerKind {
    /// Initiated explicitly by the user via IDE, tray, CLI, or MCP.
    #[default]
    Manual,
    /// Initiated automatically by live `am_crash` detection (debounced).
    CrashAutoSave,
}

/// Persistent state of a two-phase save job (`save_jobs` table).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveJobState {
    /// Phase-1 ring chunks have been durably pinned against eviction.
    Pinned,
    /// Waiting up to `drain_timeout_ms` for post-trigger in-flight stream boundaries.
    Draining,
    /// Writing files into staging directory, `fsync`ing, and atomically publishing.
    Materializing,
    /// Materialization failed; pinned chunks remain protected for retry/retarget/discard.
    FailedRetryable,
    /// Bundle was atomically committed and pins were released.
    Completed,
    /// User explicitly discarded the save job and pins were released.
    Discarded,
}

impl SaveJobState {
    /// Returns `true` if `self` is a terminal state (`Completed` or `Discarded`).
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Discarded)
    }

    /// Returns `true` if transitioning directly from `self` to `next` is permitted.
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Pinned,
                Self::Draining | Self::FailedRetryable | Self::Discarded
            ) | (
                Self::Draining,
                Self::Materializing | Self::FailedRetryable | Self::Discarded
            ) | (Self::Materializing, Self::Completed | Self::FailedRetryable)
                | (Self::FailedRetryable, Self::Materializing | Self::Discarded)
        )
    }
}

/// Transition triggers applied to a [`SaveJob`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SaveJobEvent {
    /// Move from `Pinned` to `Draining`.
    BeginDrain,
    /// Move from `Draining` to `Materializing`.
    BeginMaterialize,
    /// Move from `Materializing` to `Completed` and release pinned chunks.
    Complete,
    /// Record a retryable failure while keeping pinned chunk IDs intact.
    FailRetryable {
        /// Diagnostic failure description.
        error_message: String,
    },
    /// Retry materialization to the same destination from `FailedRetryable`.
    Retry,
    /// Retry materialization to a new destination directory from `FailedRetryable`.
    Retarget {
        /// Replacement destination path.
        new_destination_path: String,
    },
    /// Explicitly abandon the save job and release pinned chunk IDs.
    Discard,
}

impl SaveJobEvent {
    const fn label(&self) -> &'static str {
        match self {
            Self::BeginDrain => "begin_drain",
            Self::BeginMaterialize => "begin_materialize",
            Self::Complete => "complete",
            Self::FailRetryable { .. } => "fail_retryable",
            Self::Retry => "retry",
            Self::Retarget { .. } => "retarget",
            Self::Discard => "discard",
        }
    }
}

/// Stateful save job enforcing pin preservation across retryable failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveJob {
    /// Unique job identifier.
    pub job_id: String,
    /// Current job lifecycle state.
    pub state: SaveJobState,
    /// Destination path for the materialized bundle.
    pub destination_path: String,
    /// Ring chunk IDs pinned by Phase 1 until completion or explicit discard.
    pub pinned_chunk_ids: Vec<String>,
    /// Last failure reason when in `FailedRetryable`.
    pub last_error: Option<String>,
}

impl SaveJob {
    /// Creates a new save job in [`SaveJobState::Pinned`] with the given pinned chunk IDs.
    pub fn new(
        job_id: impl Into<String>,
        destination_path: impl Into<String>,
        pinned_chunk_ids: Vec<String>,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            state: SaveJobState::Pinned,
            destination_path: destination_path.into(),
            pinned_chunk_ids,
            last_error: None,
        }
    }

    /// Returns `true` while this save job still holds live ring chunk pins.
    pub fn has_pinned_chunks(&self) -> bool {
        !self.pinned_chunk_ids.is_empty()
    }

    /// Applies a [`SaveJobEvent`] according to the strict state transition table.
    ///
    /// # Errors
    ///
    /// Returns [`StateTransitionError`] if the transition is disallowed or a
    /// retarget destination path is empty.
    pub fn apply(&mut self, event: SaveJobEvent) -> Result<SaveJobState, StateTransitionError> {
        let label = event.label();
        match (self.state, event) {
            (SaveJobState::Pinned, SaveJobEvent::BeginDrain) => {
                self.state = SaveJobState::Draining;
            }
            (SaveJobState::Draining, SaveJobEvent::BeginMaterialize) => {
                self.state = SaveJobState::Materializing;
            }
            (SaveJobState::Materializing, SaveJobEvent::Complete) => {
                self.state = SaveJobState::Completed;
                self.pinned_chunk_ids.clear();
                self.last_error = None;
            }
            (
                SaveJobState::Pinned | SaveJobState::Draining | SaveJobState::Materializing,
                SaveJobEvent::FailRetryable { error_message },
            ) => {
                // Critical invariant: pinned_chunk_ids MUST stay intact across FailedRetryable!
                self.state = SaveJobState::FailedRetryable;
                self.last_error = Some(error_message);
            }
            (SaveJobState::FailedRetryable, SaveJobEvent::Retry) => {
                self.state = SaveJobState::Materializing;
                self.last_error = None;
            }
            (
                SaveJobState::FailedRetryable,
                SaveJobEvent::Retarget {
                    new_destination_path,
                },
            ) => {
                if new_destination_path.trim().is_empty() {
                    return Err(StateTransitionError::EmptyDestinationPath);
                }
                self.destination_path = new_destination_path;
                self.state = SaveJobState::Materializing;
                self.last_error = None;
            }
            (
                SaveJobState::Pinned | SaveJobState::Draining | SaveJobState::FailedRetryable,
                SaveJobEvent::Discard,
            ) => {
                self.state = SaveJobState::Discarded;
                self.pinned_chunk_ids.clear();
            }
            (from, _) => {
                return Err(StateTransitionError::InvalidSaveJobTransition { from, event: label });
            }
        }
        Ok(self.state)
    }
}

/// Lifecycle state of a cataloged bundle (`bundles.state`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleLifecycleState {
    /// Normal active bundle visible to the user.
    Active,
    /// Logically deleted; physical files remain untouched until GC conditions hold.
    Trashed,
    /// Physical deletion is in progress after lease and job checks passed.
    Purging,
}

impl BundleLifecycleState {
    /// Moves an `Active` bundle into `Trashed` (logical delete only).
    ///
    /// # Errors
    ///
    /// Returns [`StateTransitionError::InvalidBundleTransition`] if not `Active`.
    pub const fn trash(self) -> Result<Self, StateTransitionError> {
        match self {
            Self::Active => Ok(Self::Trashed),
            other => Err(StateTransitionError::InvalidBundleTransition {
                from: other,
                to: Self::Trashed,
            }),
        }
    }

    /// Restores a `Trashed` bundle back to `Active`.
    ///
    /// # Errors
    ///
    /// Returns [`StateTransitionError::InvalidBundleTransition`] if not `Trashed`.
    pub const fn restore(self) -> Result<Self, StateTransitionError> {
        match self {
            Self::Trashed => Ok(Self::Active),
            other => Err(StateTransitionError::InvalidBundleTransition {
                from: other,
                to: Self::Active,
            }),
        }
    }

    /// Returns `true` strictly when the bundle is `Trashed` with zero active
    /// leases and zero active jobs.
    pub const fn can_purge(self, active_leases: usize, active_jobs: usize) -> bool {
        matches!(self, Self::Trashed) && active_leases == 0 && active_jobs == 0
    }

    /// Transitions `Trashed -> Purging` only when [`Self::can_purge`] holds.
    ///
    /// # Errors
    ///
    /// Returns [`StateTransitionError::PurgeBlocked`] if the bundle is not
    /// `Trashed` or still has active leases or jobs.
    pub const fn begin_purge(
        self,
        active_leases: usize,
        active_jobs: usize,
    ) -> Result<Self, StateTransitionError> {
        if self.can_purge(active_leases, active_jobs) {
            Ok(Self::Purging)
        } else {
            Err(StateTransitionError::PurgeBlocked {
                state: self,
                active_leases,
                active_jobs,
            })
        }
    }
}

/// Disk and retention budget policy for the Zone-1 live ring store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RingBudgetPolicy {
    /// Maximum total bytes held in the live ring store (`1.5 GiB` default).
    pub max_ring_bytes: u64,
    /// Maximum retention duration target in nanoseconds (`10 min` default).
    pub max_ring_duration_ns: u64,
    /// Minimum free disk space required on the host filesystem (`3 GiB` default).
    pub min_free_disk_bytes: u64,
}

impl RingBudgetPolicy {
    /// Creates a policy with the default SPEC-02 ring quotas and 3 GiB safety floor.
    pub const fn new() -> Self {
        Self {
            max_ring_bytes: DEFAULT_MAX_RING_BYTES,
            max_ring_duration_ns: DEFAULT_MAX_RING_DURATION_NS,
            min_free_disk_bytes: DEFAULT_MIN_FREE_DISK_BYTES,
        }
    }
}

impl Default for RingBudgetPolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Write state of a live ring chunk.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingChunkStatus {
    /// Chunk is actively being appended to; never eligible for eviction.
    Open,
    /// Chunk is finalized and immutable; eligible for eviction if unpinned.
    Closed,
}

/// Pin protection state of a live ring chunk.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinStatus {
    /// No active save job pins this chunk.
    Unpinned,
    /// Protected by one or more Phase-1 save jobs; MUST NEVER be evicted.
    Pinned,
}

/// Metadata describing a single Zone-1 live ring chunk for eviction planning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RingChunkMeta {
    /// Unique chunk identifier.
    pub chunk_id: String,
    /// Host `PcContinuousMono` creation timestamp in nanoseconds (oldest-first ordering).
    pub created_cont_ns: i64,
    /// Approximate media/stream duration covered by this chunk in nanoseconds.
    pub duration_ns: u64,
    /// On-disk size of this chunk in bytes.
    pub bytes: u64,
    /// Whether this chunk is open for writes or closed.
    pub status: RingChunkStatus,
    /// Whether this chunk is pinned by a save job.
    pub pin: PinStatus,
}

impl RingChunkMeta {
    /// Returns `true` iff this chunk is closed and unpinned (eligible for eviction).
    pub const fn is_evictable(&self) -> bool {
        matches!(self.status, RingChunkStatus::Closed) && matches!(self.pin, PinStatus::Unpinned)
    }
}

/// Decision produced by [`RingEvictionPlanner::plan`] before accepting new ring data.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RingWriteDecision {
    /// New write is permitted after evicting `evict_chunk_ids` (all unpinned and closed).
    Allow {
        /// Ordered list of oldest unpinned closed chunk IDs to evict.
        evict_chunk_ids: Vec<String>,
    },
    /// Host free space is below `min_free_disk_bytes`; stop new capture and protect pinned data.
    StopStoragePressure {
        /// Observed free disk bytes on the host filesystem.
        host_free_bytes: u64,
        /// Configured minimum free disk floor.
        min_free_disk_bytes: u64,
        /// Total bytes held by pinned chunks (preserved untouched).
        pinned_bytes: u64,
    },
    /// The incoming chunk exceeds the entire ring budget on its own.
    ChunkTooLarge,
    /// Ring quota is full with pinned or open chunks; stop new capture without evicting pins.
    StopRingFullPinned {
        /// Remaining ring bytes after considering all evictable chunks.
        remaining_bytes: u64,
        /// Size of the rejected incoming write.
        incoming_bytes: u64,
        /// Configured ring byte cap.
        max_ring_bytes: u64,
        /// Total bytes held by pinned chunks (preserved untouched).
        pinned_bytes: u64,
    },
}

/// Pure eviction planner for Zone-1 live ring chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingEvictionPlanner {
    policy: RingBudgetPolicy,
}

impl RingEvictionPlanner {
    /// Creates a planner enforcing `policy`.
    pub const fn new(policy: RingBudgetPolicy) -> Self {
        Self { policy }
    }

    /// Returns the configured budget policy.
    pub const fn policy(&self) -> RingBudgetPolicy {
        self.policy
    }

    /// Evaluates whether an incoming ring write of `incoming_bytes` can proceed
    /// given current `chunks` and `host_free_bytes`.
    ///
    /// Guarantees:
    /// - When `host_free_bytes < min_free_disk_bytes` (or writing `incoming_bytes`
    ///   would drop below the floor), immediately emits [`RingWriteDecision::StopStoragePressure`]
    ///   without evicting any chunks.
    /// - Never evicts [`PinStatus::Pinned`] or [`RingChunkStatus::Open`] chunks.
    /// - Evicts oldest closed unpinned chunks first when ring byte or duration
    ///   quotas would be exceeded.
    pub fn plan(
        &self,
        chunks: &[RingChunkMeta],
        incoming_bytes: u64,
        host_free_bytes: u64,
    ) -> RingWriteDecision {
        let mut order: Vec<&RingChunkMeta> = chunks.iter().collect();
        match self.plan_refs(&mut order, incoming_bytes, host_free_bytes) {
            RingPlan::Allow { evict_count } => {
                let mut evict_chunk_ids = Vec::new();
                for candidate in &order[..evict_count] {
                    evict_chunk_ids.push(candidate.chunk_id.clone());
                }
                RingWriteDecision::Allow { evict_chunk_ids }
            }
            RingPlan::StopStoragePressure { pinned_bytes } => {
                RingWriteDecision::StopStoragePressure {
                    host_free_bytes,
                    min_free_disk_bytes: self.policy.min_free_disk_bytes,
                    pinned_bytes,
                }
            }
            RingPlan::ChunkTooLarge => RingWriteDecision::ChunkTooLarge,
            RingPlan::StopRingFullPinned {
                remaining_bytes,
                pinned_bytes,
            } => RingWriteDecision::StopRingFullPinned {
                remaining_bytes,
                incoming_bytes,
                max_ring_bytes: self.policy.max_ring_bytes,
                pinned_bytes,
            },
        }
    }

    /// Allocation-free core of [`Self::plan`]. `chunks` holds every chunk on entry; on
    /// [`RingPlan::Allow`] its first `evict_count` entries are the evicted chunks, oldest first
    /// by `(created_cont_ns, chunk_id)`.
    pub(crate) fn plan_refs(
        &self,
        chunks: &mut [&RingChunkMeta],
        incoming_bytes: u64,
        host_free_bytes: u64,
    ) -> RingPlan {
        let pinned_bytes: u64 = chunks
            .iter()
            .filter(|c| c.pin == PinStatus::Pinned)
            .fold(0u64, |acc, c| acc.saturating_add(c.bytes));

        if host_free_bytes < self.policy.min_free_disk_bytes
            || host_free_bytes.saturating_sub(incoming_bytes) < self.policy.min_free_disk_bytes
        {
            return RingPlan::StopStoragePressure { pinned_bytes };
        }

        let mut total_bytes: u128 = chunks.iter().map(|c| u128::from(c.bytes)).sum();
        let mut total_duration_ns: u128 = chunks.iter().map(|c| u128::from(c.duration_ns)).sum();
        if incoming_bytes > self.policy.max_ring_bytes {
            return RingPlan::ChunkTooLarge;
        }

        let mut evictable_count = 0;
        for i in 0..chunks.len() {
            if chunks[i].is_evictable() {
                chunks[evictable_count] = chunks[i];
                evictable_count += 1;
            }
        }
        let evictable = &mut chunks[..evictable_count];
        evictable.sort_by(|a, b| {
            a.created_cont_ns
                .cmp(&b.created_cont_ns)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });

        let mut evict_count = 0;
        for candidate in &*evictable {
            let over_bytes =
                total_bytes + u128::from(incoming_bytes) > u128::from(self.policy.max_ring_bytes);
            let over_duration = total_duration_ns > u128::from(self.policy.max_ring_duration_ns);
            if !over_bytes && !over_duration {
                break;
            }
            evict_count += 1;
            total_bytes -= u128::from(candidate.bytes);
            total_duration_ns -= u128::from(candidate.duration_ns);
        }

        if total_bytes + u128::from(incoming_bytes) > u128::from(self.policy.max_ring_bytes) {
            return RingPlan::StopRingFullPinned {
                remaining_bytes: u64::try_from(total_bytes).unwrap_or(u64::MAX),
                pinned_bytes,
            };
        }

        RingPlan::Allow { evict_count }
    }
}

/// Scalar outcome of [`RingEvictionPlanner::plan_refs`]; [`RingEvictionPlanner::plan`]
/// maps it to a [`RingWriteDecision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RingPlan {
    /// Write allowed after evicting the first `evict_count` entries of the order buffer.
    Allow {
        /// Number of evicted chunks.
        evict_count: usize,
    },
    /// Host free space is below the floor.
    StopStoragePressure {
        /// Total bytes held by pinned chunks.
        pinned_bytes: u64,
    },
    /// The incoming chunk exceeds the whole ring budget.
    ChunkTooLarge,
    /// Ring quota stays full with only pinned or open chunks left.
    StopRingFullPinned {
        /// Remaining ring bytes after every eviction.
        remaining_bytes: u64,
        /// Total bytes held by pinned chunks.
        pinned_bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        BundleLifecycleState, PinStatus, RingBudgetPolicy, RingChunkMeta, RingChunkStatus,
        RingEvictionPlanner, RingWriteDecision, SaveJob, SaveJobEvent, SaveJobState,
        StateTransitionError,
    };

    #[test]
    fn save_job_preserves_pins_across_failure_retry_and_retarget() {
        let mut job = SaveJob::new("job-1", "/tmp/dest-a", vec!["c1".into(), "c2".into()]);
        assert_eq!(
            job.apply(SaveJobEvent::BeginDrain),
            Ok(SaveJobState::Draining),
            "Pinned -> Draining must succeed"
        );
        assert_eq!(
            job.apply(SaveJobEvent::BeginMaterialize),
            Ok(SaveJobState::Materializing),
            "Draining -> Materializing must succeed"
        );
        assert_eq!(
            job.apply(SaveJobEvent::FailRetryable {
                error_message: "disk full".into(),
            }),
            Ok(SaveJobState::FailedRetryable),
            "Materializing -> FailedRetryable must succeed"
        );
        assert_eq!(
            job.pinned_chunk_ids,
            vec!["c1".to_owned(), "c2".to_owned()],
            "FailedRetryable must keep pinned chunk IDs intact"
        );

        assert_eq!(
            job.apply(SaveJobEvent::Retarget {
                new_destination_path: "/Volumes/External/dest-b".into(),
            }),
            Ok(SaveJobState::Materializing),
            "FailedRetryable -> Retarget -> Materializing must succeed"
        );
        assert_eq!(
            job.destination_path, "/Volumes/External/dest-b",
            "destination_path must update on Retarget"
        );
        assert_eq!(
            job.pinned_chunk_ids.len(),
            2,
            "pins must remain intact during retry/retarget"
        );

        assert_eq!(
            job.apply(SaveJobEvent::Complete),
            Ok(SaveJobState::Completed),
            "Materializing -> Completed must succeed"
        );
        assert!(
            !job.has_pinned_chunks(),
            "Completed job must release pinned chunks"
        );
        assert!(
            matches!(
                job.apply(SaveJobEvent::Retry),
                Err(StateTransitionError::InvalidSaveJobTransition {
                    from: SaveJobState::Completed,
                    ..
                })
            ),
            "terminal Completed state must reject further transitions"
        );
    }

    #[test]
    fn bundle_purge_requires_trashed_and_zero_leases_and_zero_jobs() {
        let active = BundleLifecycleState::Active;
        assert!(!active.can_purge(0, 0), "Active bundle can never be purged");
        assert!(
            active.begin_purge(0, 0).is_err(),
            "Active bundle must block begin_purge"
        );

        let trashed = active.trash().expect("Active -> Trashed must succeed");
        assert!(
            !trashed.can_purge(1, 0),
            "Trashed bundle with active lease cannot be purged"
        );
        assert!(
            !trashed.can_purge(0, 1),
            "Trashed bundle with active job cannot be purged"
        );
        assert!(
            trashed.can_purge(0, 0),
            "Trashed bundle with zero leases and zero jobs may be purged"
        );
        assert_eq!(
            trashed.begin_purge(0, 0),
            Ok(BundleLifecycleState::Purging),
            "Trashed -> Purging must succeed when unreferenced"
        );
    }

    #[test]
    fn ring_planner_evicts_oldest_unpinned_closed_and_stops_on_storage_pressure() {
        let policy = RingBudgetPolicy {
            max_ring_bytes: 1_000,
            max_ring_duration_ns: 600_000_000_000,
            min_free_disk_bytes: 3_000,
        };
        let planner = RingEvictionPlanner::new(policy);

        let chunks = vec![
            RingChunkMeta {
                chunk_id: "c-pinned-oldest".into(),
                created_cont_ns: 10,
                duration_ns: 100,
                bytes: 400,
                status: RingChunkStatus::Closed,
                pin: PinStatus::Pinned,
            },
            RingChunkMeta {
                chunk_id: "c-unpinned-old".into(),
                created_cont_ns: 20,
                duration_ns: 100,
                bytes: 400,
                status: RingChunkStatus::Closed,
                pin: PinStatus::Unpinned,
            },
            RingChunkMeta {
                chunk_id: "c-open-new".into(),
                created_cont_ns: 30,
                duration_ns: 100,
                bytes: 150,
                status: RingChunkStatus::Open,
                pin: PinStatus::Unpinned,
            },
        ];

        // Normal quota eviction: skips pinned oldest and open chunk, evicts c-unpinned-old.
        let decision = planner.plan(&chunks, 300, 10_000);
        assert_eq!(
            decision,
            RingWriteDecision::Allow {
                evict_chunk_ids: vec!["c-unpinned-old".into()],
            },
            "must evict only oldest unpinned closed chunk"
        );

        // Storage pressure: host_free < min_free_disk_bytes -> StopStoragePressure, zero eviction.
        let pressure = planner.plan(&chunks, 100, 2_500);
        assert_eq!(
            pressure,
            RingWriteDecision::StopStoragePressure {
                host_free_bytes: 2_500,
                min_free_disk_bytes: 3_000,
                pinned_bytes: 400,
            },
            "low host free space must stop capture without evicting pinned chunks"
        );
    }
}
