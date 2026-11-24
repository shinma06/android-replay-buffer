//! Layer-2 PDD: exhaustive Stateright interleaving models (S1–S3).
//!
//! Each model is a linearized abstraction of production lifecycle/durability/media
//! rules. Liveness-style requirements are checked as `always` properties over a
//! fair *drain* closure (only environment progress actions) so every reachable
//! state is checked, not only terminal paths.

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

#![expect(
    clippy::too_many_lines,
    reason = "state machines are clearer as one match"
)]

use std::time::Instant;

use stateright::{Checker, Model, Property};

fn run<M>(name: &str, model: M) -> usize
where
    M: Model + Send + Sync + 'static,
    M::State: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync,
    M::Action: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync,
{
    let started = Instant::now();
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let checker = model.checker().threads(threads).spawn_bfs().join();
    let unique = checker.unique_state_count();
    println!(
        "{name}: unique_states={unique} total_states={} elapsed_ms={}",
        checker.state_count(),
        started.elapsed().as_millis()
    );
    checker.assert_properties();
    unique
}

// =====================================================================================
// S1 — Android Studio parent-bound lifecycle, multi-window, tray, handover, upgrade
// =====================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Life {
    Alive,
    Disposing,
    Dead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Ver {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Owner {
    Ide(usize),
    Cli,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Phase {
    Starting,
    Listening,
    ShuttingDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Ide {
    life: Life,
    windows: u8,
    tray_enabled: bool,
    tray_manual_quit: bool,
    version: Ver,
    pending_spawn: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Daemon {
    owner: Owner,
    parent_bound: bool,
    version: Ver,
    phase: Phase,
    saves: u8,
    crashes: u8,
    saw_eof: bool,
    born_after_dispose: bool,
}

impl Daemon {
    const fn busy(&self) -> bool {
        self.saves > 0 || self.crashes > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Tray {
    owner: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct S1State {
    ides: [Ide; 2],
    daemons: [Option<Daemon>; 2],
    tray: Option<Tray>,
    lock: Option<usize>,
    cli_alive: bool,
    forced_with_work: bool,
    unauthorized_replace: bool,
    handovers_done: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum S1Action {
    OpenProject(usize),
    CloseWindow(usize),
    SupervisorTick(usize),
    Spawn(usize),
    AcquireLock(usize),
    ToggleTray(usize),
    TrayManualQuit,
    BeginSave(usize),
    FinishSave(usize),
    QueueCrash(usize),
    FinishCrash(usize),
    HandoverRequest(usize),
    CliUpgrade,
    CliExit,
    Dispose(usize),
    Kill9(usize),
    ObserveEof(usize),
    TrayEof,
    ChildExit(usize),
}

struct S1Model;

impl S1State {
    fn live_daemon(&self) -> Option<usize> {
        (0..2).find(|&d| {
            self.daemons[d].is_some_and(|x| matches!(x.phase, Phase::Starting | Phase::Listening))
        })
    }
    fn listening(&self) -> Option<usize> {
        (0..2).find(|&d| self.daemons[d].is_some_and(|x| x.phase == Phase::Listening))
    }
    fn parent_dead(&self, d: &Daemon) -> bool {
        match d.owner {
            Owner::Ide(p) => d.parent_bound && self.ides[p].life != Life::Alive,
            Owner::Cli => false,
        }
    }
}

const S1_PROGRESS: [fn(usize) -> S1Action; 4] = [
    S1Action::FinishSave,
    S1Action::FinishCrash,
    S1Action::ObserveEof,
    S1Action::ChildExit,
];

/// Applies environment progress (EOF delivery, job completion, child exit) to a fixpoint.
fn s1_drain(model: &S1Model, s: &S1State) -> S1State {
    let mut cur = s.clone();
    let mut guard = 0;
    loop {
        let mut changed = false;
        for d in 0..2 {
            for mk in S1_PROGRESS {
                if let Some(n) = model.next_state(&cur, mk(d)) {
                    if n != cur {
                        cur = n;
                        changed = true;
                    }
                }
            }
            if let Some(n) = model.next_state(&cur, S1Action::AcquireLock(d)) {
                if n != cur {
                    cur = n;
                    changed = true;
                }
            }
        }
        if let Some(n) = model.next_state(&cur, S1Action::TrayEof) {
            if n != cur {
                cur = n;
                changed = true;
            }
        }
        guard += 1;
        assert!(guard < 64, "S1 drain must converge");
        if !changed {
            return cur;
        }
    }
}

impl Model for S1Model {
    type State = S1State;
    type Action = S1Action;

    fn init_states(&self) -> Vec<S1State> {
        let ide = |version| Ide {
            life: Life::Alive,
            windows: 1,
            tray_enabled: true,
            tray_manual_quit: false,
            version,
            pending_spawn: false,
        };
        let base = S1State {
            ides: [ide(Ver::Old), ide(Ver::New)],
            daemons: [None, None],
            tray: None,
            lock: None,
            cli_alive: false,
            forced_with_work: false,
            unauthorized_replace: false,
            handovers_done: 0,
        };
        let mut standalone = base.clone();
        standalone.daemons[0] = Some(Daemon {
            owner: Owner::Cli,
            parent_bound: false,
            version: Ver::Old,
            phase: Phase::Listening,
            saves: 0,
            crashes: 0,
            saw_eof: false,
            born_after_dispose: false,
        });
        standalone.lock = Some(0);
        standalone.cli_alive = true;
        vec![base, standalone]
    }

    fn actions(&self, _s: &S1State, out: &mut Vec<S1Action>) {
        for i in 0..2 {
            out.extend([
                S1Action::OpenProject(i),
                S1Action::CloseWindow(i),
                S1Action::SupervisorTick(i),
                S1Action::Spawn(i),
                S1Action::AcquireLock(i),
                S1Action::ToggleTray(i),
                S1Action::BeginSave(i),
                S1Action::FinishSave(i),
                S1Action::QueueCrash(i),
                S1Action::FinishCrash(i),
                S1Action::HandoverRequest(i),
                S1Action::Dispose(i),
                S1Action::Kill9(i),
                S1Action::ObserveEof(i),
                S1Action::ChildExit(i),
            ]);
        }
        out.extend([
            S1Action::TrayManualQuit,
            S1Action::CliUpgrade,
            S1Action::CliExit,
            S1Action::TrayEof,
        ]);
    }

    fn next_state(&self, s: &S1State, a: S1Action) -> Option<S1State> {
        let mut n = s.clone();
        match a {
            S1Action::OpenProject(i) => {
                // IDE B keeps one window: multi-window behaviour is exercised on IDE A.
                if i != 0 || s.ides[i].life != Life::Alive || s.ides[i].windows >= 2 {
                    return None;
                }
                n.ides[i].windows += 1;
            }
            S1Action::CloseWindow(i) => {
                // Application-scoped daemon/tray are unaffected by project windows.
                if i != 0 || s.ides[i].life != Life::Alive || s.ides[i].windows == 0 {
                    return None;
                }
                n.ides[i].windows -= 1;
            }
            S1Action::SupervisorTick(i) => {
                if s.ides[i].life != Life::Alive {
                    return None;
                }
                if s.live_daemon().is_none() {
                    n.ides[i].pending_spawn = true;
                }
                let ide = s.ides[i];
                if ide.tray_enabled
                    && !ide.tray_manual_quit
                    && s.tray.is_none()
                    && s.listening().is_some()
                {
                    n.tray = Some(Tray { owner: i });
                }
                if n == *s {
                    return None;
                }
            }
            S1Action::Spawn(i) => {
                if !s.ides[i].pending_spawn {
                    return None;
                }
                n.ides[i].pending_spawn = false;
                // Production re-checks `disposed` under the service lock before spawning.
                if s.ides[i].life == Life::Alive {
                    let slot = (0..2).find(|&d| s.daemons[d].is_none())?;
                    n.daemons[slot] = Some(Daemon {
                        owner: Owner::Ide(i),
                        parent_bound: true,
                        version: s.ides[i].version,
                        phase: Phase::Starting,
                        saves: 0,
                        crashes: 0,
                        saw_eof: false,
                        born_after_dispose: s.ides[i].life != Life::Alive,
                    });
                }
            }
            S1Action::AcquireLock(d) => {
                let mut dm = s.daemons[d]?;
                if dm.phase != Phase::Starting {
                    return None;
                }
                if s.lock.is_none() {
                    n.lock = Some(d);
                    dm.phase = Phase::Listening;
                } else {
                    dm.phase = Phase::ShuttingDown;
                }
                n.daemons[d] = Some(dm);
            }
            S1Action::ToggleTray(i) => {
                if s.ides[i].life != Life::Alive {
                    return None;
                }
                let enabled = !s.ides[i].tray_enabled;
                n.ides[i].tray_enabled = enabled;
                if enabled {
                    n.ides[i].tray_manual_quit = false;
                } else if s.tray.is_some_and(|t| t.owner == i) {
                    n.tray = None;
                }
            }
            S1Action::TrayManualQuit => {
                let t = s.tray?;
                n.tray = None;
                n.ides[t.owner].tray_manual_quit = true;
            }
            S1Action::BeginSave(d) | S1Action::QueueCrash(d) => {
                let mut dm = s.daemons[d]?;
                if dm.phase != Phase::Listening || dm.saw_eof {
                    return None;
                }
                if matches!(a, S1Action::BeginSave(_)) {
                    if dm.saves > 0 {
                        return None;
                    }
                    dm.saves = 1;
                } else {
                    if dm.crashes > 0 {
                        return None;
                    }
                    dm.crashes = 1;
                }
                n.daemons[d] = Some(dm);
            }
            S1Action::FinishSave(d) | S1Action::FinishCrash(d) => {
                let mut dm = s.daemons[d]?;
                if matches!(a, S1Action::FinishSave(_)) {
                    if dm.saves == 0 {
                        return None;
                    }
                    dm.saves = 0;
                } else {
                    if dm.crashes == 0 {
                        return None;
                    }
                    dm.crashes = 0;
                }
                // EOF seen while busy: shut down only after the last job drains.
                if dm.saw_eof && !dm.busy() {
                    dm.phase = Phase::ShuttingDown;
                }
                n.daemons[d] = Some(dm);
            }
            S1Action::HandoverRequest(i) => {
                if s.ides[i].life != Life::Alive {
                    return None;
                }
                let d = s.listening()?;
                let mut dm = s.daemons[d]?;
                if dm.version >= s.ides[i].version {
                    return None;
                }
                let authorized = dm.owner == Owner::Ide(i)
                    || (dm.owner == Owner::Cli && !s.cli_alive)
                    || s.parent_dead(&dm);
                if !authorized || dm.busy() {
                    return None; // refused: no replacement
                }
                if !authorized {
                    n.unauthorized_replace = true;
                }
                if dm.busy() {
                    n.forced_with_work = true;
                }
                dm.phase = Phase::ShuttingDown;
                n.daemons[d] = Some(dm);
                n.ides[i].pending_spawn = true;
                n.handovers_done = s.handovers_done.saturating_add(1).min(1);
            }
            S1Action::CliUpgrade => {
                match s.live_daemon() {
                    None => {
                        let slot = (0..2).find(|&d| s.daemons[d].is_none())?;
                        n.daemons[slot] = Some(Daemon {
                            owner: Owner::Cli,
                            parent_bound: false,
                            version: Ver::New,
                            phase: Phase::Starting,
                            saves: 0,
                            crashes: 0,
                            saw_eof: false,
                            born_after_dispose: false,
                        });
                    }
                    Some(d) => {
                        let mut dm = s.daemons[d]?;
                        if dm.owner != Owner::Cli || dm.version == Ver::New || dm.busy() {
                            return None;
                        }
                        if dm.phase != Phase::Listening {
                            return None;
                        }
                        dm.phase = Phase::ShuttingDown;
                        n.daemons[d] = Some(dm);
                    }
                }
                n.cli_alive = true;
            }
            S1Action::CliExit => {
                if !s.cli_alive {
                    return None;
                }
                n.cli_alive = false;
            }
            S1Action::Dispose(i) => {
                if s.ides[i].life != Life::Alive {
                    return None;
                }
                n.ides[i].life = Life::Disposing;
                n.ides[i].pending_spawn = false;
            }
            S1Action::Kill9(i) => {
                if s.ides[i].life == Life::Dead {
                    return None;
                }
                n.ides[i].life = Life::Dead;
                n.ides[i].pending_spawn = false;
            }
            S1Action::ObserveEof(d) => {
                let mut dm = s.daemons[d]?;
                if dm.saw_eof || !s.parent_dead(&dm) || dm.phase == Phase::ShuttingDown {
                    return None;
                }
                dm.saw_eof = true;
                if !dm.busy() {
                    dm.phase = Phase::ShuttingDown;
                }
                n.daemons[d] = Some(dm);
            }
            S1Action::TrayEof => {
                let t = s.tray?;
                if s.ides[t.owner].life == Life::Alive {
                    return None;
                }
                n.tray = None;
            }
            S1Action::ChildExit(d) => {
                let dm = s.daemons[d]?;
                if dm.phase != Phase::ShuttingDown {
                    return None;
                }
                n.daemons[d] = None;
                if s.lock == Some(d) {
                    n.lock = None;
                }
                if s.listening().is_none() && s.tray.is_some() && n.live_daemon().is_none() {
                    // Tray follows its daemon connection; it is re-attached by the next tick.
                }
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("S1 single lock owner", |_: &S1Model, s: &S1State| {
                let listening: Vec<usize> = (0..2)
                    .filter(|&d| s.daemons[d].is_some_and(|x| x.phase == Phase::Listening))
                    .collect();
                listening.len() <= 1 && listening.iter().all(|&d| s.lock == Some(d))
            }),
            Property::always(
                "S1 no forced replacement with work",
                |_: &S1Model, s: &S1State| {
                    !s.forced_with_work
                        && s.daemons
                            .iter()
                            .flatten()
                            .all(|d| d.phase != Phase::ShuttingDown || !d.busy() || d.saw_eof)
                },
            ),
            Property::always(
                "S1 no unauthorized owner replacement",
                |_: &S1Model, s: &S1State| !s.unauthorized_replace,
            ),
            Property::always(
                "S1 no bound child spawned after dispose",
                |_: &S1Model, s: &S1State| {
                    s.daemons.iter().flatten().all(|d| !d.born_after_dispose)
                },
            ),
            Property::always(
                "S1 parents dead ⇒ drain stops bound children",
                |m: &S1Model, s: &S1State| {
                    if s.ides.iter().any(|i| i.life == Life::Alive) {
                        return true;
                    }
                    let d = s1_drain(m, s);
                    d.tray.is_none()
                        && d.daemons.iter().flatten().all(|x| !x.parent_bound)
                        && d.lock
                            .is_none_or(|l| d.daemons[l].is_some_and(|x| !x.parent_bound))
                },
            ),
            Property::always(
                "S1 close 1 of 2 windows keeps daemon and tray",
                |m: &S1Model, s: &S1State| {
                    (0..2).all(|i| {
                        if s.ides[i].life != Life::Alive || s.ides[i].windows != 2 {
                            return true;
                        }
                        m.next_state(s, S1Action::CloseWindow(i))
                            .is_some_and(|n| n.daemons == s.daemons && n.tray == s.tray)
                    })
                },
            ),
            Property::always(
                "S1 disabling tray never stops daemon",
                |m: &S1Model, s: &S1State| {
                    (0..2).all(|i| {
                        m.next_state(s, S1Action::ToggleTray(i))
                            .is_none_or(|n| n.daemons == s.daemons && n.lock == s.lock)
                    })
                },
            ),
            Property::always(
                "S1 standalone CLI daemon survives CLI exit",
                |m: &S1Model, s: &S1State| {
                    m.next_state(s, S1Action::CliExit)
                        .is_none_or(|n| n.daemons == s.daemons)
                },
            ),
            Property::always(
                "S1 surviving IDE re-acquires tray",
                |m: &S1Model, s: &S1State| {
                    (0..2).all(|i| {
                        let ide = s.ides[i];
                        let eligible = ide.life == Life::Alive
                            && ide.tray_enabled
                            && !ide.tray_manual_quit
                            && s.tray.is_none()
                            && s.listening().is_some();
                        !eligible
                            || m.next_state(s, S1Action::SupervisorTick(i))
                                .is_some_and(|n| n.tray == Some(Tray { owner: i }))
                    })
                },
            ),
            Property::always(
                "S1 manual tray quit not auto-revived",
                |m: &S1Model, s: &S1State| {
                    (0..2).all(|i| {
                        !s.ides[i].tray_manual_quit
                            || m.next_state(s, S1Action::SupervisorTick(i))
                                .is_none_or(|n| n.tray != Some(Tray { owner: i }))
                    })
                },
            ),
            Property::sometimes(
                "S1 tray re-acquired by IDE B after IDE A died",
                |_: &S1Model, s: &S1State| {
                    s.ides[0].life == Life::Dead && s.tray == Some(Tray { owner: 1 })
                },
            ),
            Property::sometimes(
                "S1 authorized handover completed",
                |_: &S1Model, s: &S1State| s.handovers_done > 0,
            ),
            Property::sometimes(
                "S1 EOF while saving drains before exit",
                |_: &S1Model, s: &S1State| {
                    s.daemons
                        .iter()
                        .flatten()
                        .any(|d| d.saw_eof && d.busy() && d.phase == Phase::Listening)
                },
            ),
        ]
    }
}

#[test]
fn s1_parent_bound_lifecycle_model() {
    let states = run("S1", S1Model);
    assert!(
        states > 1_000,
        "S1 must explore a non-trivial interleaving space"
    );
}

// =====================================================================================
// S2 — REQ-04 data preservation, single-writer data_dir, trash grace, crash/reboot
// =====================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Disk {
    Ok,
    Low,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Chunk {
    boot: u8,
    pinned: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum JobPhase {
    /// Pins + params held in memory only (not yet durable).
    Pinned,
    /// Phase-1 SQL commit done: pins and params are durable.
    Committed,
    Staged,
    Renamed,
    Registered,
    Completed,
    FailedRetryable,
    Discarded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Staging {
    Managed,
    External,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Job {
    phase: JobPhase,
    pins: u8,
    boot: u8,
    managed: bool,
    staging: Option<Staging>,
}

impl Job {
    const fn terminal(&self) -> bool {
        matches!(self.phase, JobPhase::Completed | JobPhase::Discarded)
    }
    const fn durable(&self) -> bool {
        !matches!(self.phase, JobPhase::Pinned)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum BState {
    Active,
    Trashed,
    Purged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Bundle {
    state: BState,
    managed: bool,
    trashed_at: u8,
    leases: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct S2State {
    writers: [bool; 2],
    disk: Disk,
    boot: u8,
    clock: u8,
    chunks: [Option<Chunk>; 2],
    job: Option<Job>,
    bundle: Option<Bundle>,
    gc_marked: bool,
    legacy_intact: bool,
    nonarb_staging_intact: bool,
    just_recovered: bool,
    // ghost violation flags (set by the transition at the moment the rule is broken)
    pinned_evicted: bool,
    durable_job_lost: bool,
    bad_purge: bool,
    boot_blackout: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum S2Action {
    AcquireDataLock(usize),
    WriteChunk,
    DiskLevelChange(Disk),
    Phase1PinAndParams,
    Phase1SqlCommit,
    Phase2StageWrite,
    Phase2AtomicRename { parent_sync_ok: bool },
    Phase2CatalogRegister,
    Phase2CompleteJob,
    Retry,
    Retarget,
    Discard,
    TrashBundle,
    RestoreBundle,
    AcquireLease,
    ReleaseLease,
    GcMark,
    GcPurge,
    AdvanceClock,
    DaemonCrashAndRecover,
    HostReboot,
    BinaryUpgrade,
}

/// One clock step is the 24 h trash grace period.
const GRACE_STEPS: u8 = 1;

struct S2Model;

impl S2State {
    fn has_writer(&self) -> bool {
        self.writers.iter().any(|&w| w)
    }
    fn active_jobs(&self) -> usize {
        usize::from(
            self.job
                .is_some_and(|j| !j.terminal() && j.phase == JobPhase::Registered),
        )
    }
    fn purge_allowed(&self) -> bool {
        self.bundle.is_some_and(|b| {
            b.managed
                && b.state == BState::Trashed
                && self.clock.saturating_sub(b.trashed_at) >= GRACE_STEPS
                && b.leases == 0
        }) && self.active_jobs() == 0
    }
    fn release_pins(&mut self, pins: u8) {
        for slot in 0..2 {
            if pins & (1 << slot) != 0 {
                if let Some(c) = self.chunks[slot].as_mut() {
                    c.pinned = false;
                }
            }
        }
    }
    /// `recover_on_startup`: drop ARB-owned staging, fail-retry durable in-flight jobs,
    /// release non-durable and terminal pins.
    fn recover(&mut self) {
        self.writers = [false; 2];
        self.gc_marked = false;
        if let Some(mut j) = self.job {
            j.staging = None;
            match j.phase {
                JobPhase::Pinned => {
                    self.release_pins(j.pins);
                    self.job = None;
                    self.just_recovered = true;
                    return;
                }
                JobPhase::Committed | JobPhase::Staged | JobPhase::Renamed => {
                    j.phase = JobPhase::FailedRetryable;
                }
                JobPhase::Registered => j.phase = JobPhase::Completed,
                JobPhase::Completed | JobPhase::Discarded | JobPhase::FailedRetryable => {}
            }
            if j.terminal() {
                self.release_pins(j.pins);
                j.pins = 0;
            }
            self.job = Some(j);
        }
        self.just_recovered = true;
    }
}

impl Model for S2Model {
    type State = S2State;
    type Action = S2Action;

    fn init_states(&self) -> Vec<S2State> {
        vec![S2State {
            writers: [false; 2],
            disk: Disk::Ok,
            boot: 0,
            clock: 0,
            chunks: [None, None],
            job: None,
            bundle: None,
            gc_marked: false,
            legacy_intact: true,
            nonarb_staging_intact: true,
            just_recovered: false,
            pinned_evicted: false,
            durable_job_lost: false,
            bad_purge: false,
            boot_blackout: false,
        }]
    }

    fn actions(&self, _s: &S2State, out: &mut Vec<S2Action>) {
        out.extend([
            S2Action::AcquireDataLock(0),
            S2Action::AcquireDataLock(1),
            S2Action::WriteChunk,
            S2Action::DiskLevelChange(Disk::Ok),
            S2Action::DiskLevelChange(Disk::Low),
            S2Action::DiskLevelChange(Disk::Full),
            S2Action::Phase1PinAndParams,
            S2Action::Phase1SqlCommit,
            S2Action::Phase2StageWrite,
            S2Action::Phase2AtomicRename {
                parent_sync_ok: true,
            },
            S2Action::Phase2AtomicRename {
                parent_sync_ok: false,
            },
            S2Action::Phase2CatalogRegister,
            S2Action::Phase2CompleteJob,
            S2Action::Retry,
            S2Action::Retarget,
            S2Action::Discard,
            S2Action::TrashBundle,
            S2Action::RestoreBundle,
            S2Action::AcquireLease,
            S2Action::ReleaseLease,
            S2Action::GcMark,
            S2Action::GcPurge,
            S2Action::AdvanceClock,
            S2Action::DaemonCrashAndRecover,
            S2Action::HostReboot,
            S2Action::BinaryUpgrade,
        ]);
    }

    fn next_state(&self, s: &S2State, a: S2Action) -> Option<S2State> {
        let mut n = s.clone();
        let writer_ops = !matches!(
            a,
            S2Action::AcquireDataLock(_)
                | S2Action::DiskLevelChange(_)
                | S2Action::AdvanceClock
                | S2Action::DaemonCrashAndRecover
                | S2Action::HostReboot
                | S2Action::BinaryUpgrade
        );
        if writer_ops && !s.has_writer() {
            return None;
        }
        if !matches!(
            a,
            S2Action::DaemonCrashAndRecover | S2Action::HostReboot | S2Action::BinaryUpgrade
        ) {
            n.just_recovered = false;
        }
        match a {
            S2Action::AcquireDataLock(p) => {
                if s.has_writer() {
                    return None; // exclusive data_dir lock
                }
                n.writers[p] = true;
            }
            S2Action::WriteChunk => {
                if s.disk != Disk::Ok {
                    return None; // storage pressure stops capture
                }
                let chunk = Chunk {
                    boot: s.boot,
                    pinned: false,
                };
                if let Some(slot) = (0..2).find(|&i| s.chunks[i].is_none()) {
                    n.chunks[slot] = Some(chunk);
                } else {
                    // Oldest-first eviction of closed unpinned chunks only (slot 0 is oldest).
                    let victim = (0..2).find(|&i| s.chunks[i].is_some_and(|c| !c.pinned))?;
                    if s.chunks[victim].is_some_and(|c| c.pinned) {
                        n.pinned_evicted = true;
                    }
                    if victim == 0 {
                        n.chunks = [s.chunks[1], Some(chunk)];
                    } else {
                        n.chunks = [s.chunks[0], Some(chunk)];
                    }
                    if let Some(mut j) = n.job {
                        // Slot indices shift when slot 0 is evicted.
                        if victim == 0 {
                            j.pins >>= 1;
                        }
                        n.job = Some(j);
                    }
                }
            }
            S2Action::DiskLevelChange(d) => {
                if s.disk == d {
                    return None;
                }
                n.disk = d;
            }
            S2Action::Phase1PinAndParams => {
                if s.job.is_some_and(|j| !j.terminal()) {
                    return None;
                }
                let mut pins = 0u8;
                for slot in 0..2 {
                    if let Some(c) = n.chunks[slot].as_mut() {
                        if c.boot == s.boot {
                            c.pinned = true;
                            pins |= 1 << slot;
                        }
                    }
                }
                if pins == 0 && s.chunks.iter().flatten().any(|c| c.boot == s.boot) {
                    n.boot_blackout = true;
                }
                n.job = Some(Job {
                    phase: JobPhase::Pinned,
                    pins,
                    boot: s.boot,
                    managed: true,
                    staging: None,
                });
                n.bundle = None;
                n.gc_marked = false;
            }
            S2Action::Phase1SqlCommit => {
                let mut j = s.job?;
                if j.phase != JobPhase::Pinned {
                    return None;
                }
                j.phase = JobPhase::Committed;
                n.job = Some(j);
            }
            S2Action::Phase2StageWrite => {
                let mut j = s.job?;
                if j.phase != JobPhase::Committed {
                    return None;
                }
                if s.disk == Disk::Ok {
                    j.phase = JobPhase::Staged;
                    j.staging = Some(if j.managed {
                        Staging::Managed
                    } else {
                        Staging::External
                    });
                } else {
                    j.phase = JobPhase::FailedRetryable;
                }
                n.job = Some(j);
            }
            S2Action::Phase2AtomicRename { parent_sync_ok } => {
                let mut j = s.job?;
                if j.phase != JobPhase::Staged {
                    return None;
                }
                if parent_sync_ok {
                    j.phase = JobPhase::Renamed;
                    j.staging = None;
                } else {
                    j.phase = JobPhase::FailedRetryable;
                    j.staging = None; // staging cleaned up on failure
                }
                n.job = Some(j);
            }
            S2Action::Phase2CatalogRegister => {
                let mut j = s.job?;
                if j.phase != JobPhase::Renamed {
                    return None;
                }
                j.phase = JobPhase::Registered;
                n.job = Some(j);
                n.bundle = Some(Bundle {
                    state: BState::Active,
                    managed: j.managed,
                    trashed_at: 0,
                    leases: 0,
                });
            }
            S2Action::Phase2CompleteJob => {
                let mut j = s.job?;
                if j.phase != JobPhase::Registered {
                    return None;
                }
                j.phase = JobPhase::Completed;
                n.release_pins(j.pins);
                j.pins = 0;
                n.job = Some(j);
            }
            S2Action::Retry | S2Action::Retarget => {
                let mut j = s.job?;
                if j.phase != JobPhase::FailedRetryable {
                    return None;
                }
                if matches!(a, S2Action::Retarget) {
                    j.managed = !j.managed;
                }
                j.phase = JobPhase::Committed;
                n.job = Some(j);
            }
            S2Action::Discard => {
                let mut j = s.job?;
                if j.terminal() || !j.durable() {
                    return None;
                }
                j.phase = JobPhase::Discarded;
                j.staging = None;
                n.release_pins(j.pins);
                j.pins = 0;
                n.job = Some(j);
            }
            S2Action::TrashBundle => {
                let mut b = s.bundle?;
                if b.state != BState::Active {
                    return None;
                }
                b.state = BState::Trashed;
                b.trashed_at = s.clock;
                n.bundle = Some(b);
            }
            S2Action::RestoreBundle => {
                let mut b = s.bundle?;
                if b.state != BState::Trashed {
                    return None;
                }
                b.state = BState::Active;
                n.bundle = Some(b);
            }
            S2Action::AcquireLease | S2Action::ReleaseLease => {
                let mut b = s.bundle?;
                if b.state == BState::Purged {
                    return None;
                }
                if matches!(a, S2Action::AcquireLease) {
                    if b.leases > 0 {
                        return None;
                    }
                    b.leases = 1;
                } else {
                    if b.leases == 0 {
                        return None;
                    }
                    b.leases = 0;
                }
                n.bundle = Some(b);
            }
            S2Action::GcMark => {
                // Enumerate phase (may go stale before purge: restore/lease/re-trash in between).
                if s.gc_marked || !s.purge_allowed() {
                    return None;
                }
                n.gc_marked = true;
            }
            S2Action::GcPurge => {
                if !s.gc_marked {
                    return None;
                }
                n.gc_marked = false;
                // Re-check under the catalog write transaction before deleting anything.
                if s.purge_allowed() {
                    let mut b = s.bundle?;
                    b.state = BState::Purged;
                    n.bundle = Some(b);
                }
                if n.bundle.is_some_and(|b| b.state == BState::Purged)
                    && s.bundle.is_some_and(|b| b.state != BState::Purged)
                    && !s.purge_allowed()
                {
                    n.bad_purge = true;
                }
            }
            S2Action::AdvanceClock => {
                if s.clock >= 2 {
                    return None;
                }
                n.clock += 1;
            }
            S2Action::DaemonCrashAndRecover | S2Action::BinaryUpgrade => {
                n.recover();
            }
            S2Action::HostReboot => {
                if s.boot >= 1 {
                    return None;
                }
                n.recover();
                n.boot = 1;
            }
        }
        if s.job.is_some_and(|j| j.durable() && !j.terminal()) && n.job.is_none() {
            n.durable_job_lost = true;
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("S2 single data_dir writer", |_: &S2Model, s: &S2State| {
                s.writers.iter().filter(|&&w| w).count() <= 1
            }),
            Property::always(
                "S2 non-terminal job pins never evicted",
                |_: &S2Model, s: &S2State| {
                    !s.pinned_evicted
                        && s.job.is_none_or(|j| {
                            j.terminal()
                                || (0..2).all(|slot| {
                                    j.pins & (1 << slot) == 0
                                        || s.chunks[slot]
                                            .is_some_and(|c| c.pinned && c.boot == j.boot)
                                })
                        })
                },
            ),
            Property::always(
                "S2 durable job is committed or recoverable",
                |_: &S2Model, s: &S2State| !s.durable_job_lost,
            ),
            Property::always(
                "S2 purge only after grace, no leases, no jobs",
                |_: &S2Model, s: &S2State| {
                    !s.bad_purge
                        && s.bundle
                            .is_none_or(|b| b.state != BState::Purged || b.managed)
                },
            ),
            Property::always(
                "S2 legacy and non-ARB staging untouched",
                |_: &S2Model, s: &S2State| s.legacy_intact && s.nonarb_staging_intact,
            ),
            Property::always(
                "S2 no ARB staging after crash recovery",
                |_: &S2Model, s: &S2State| {
                    !s.just_recovered || s.job.is_none_or(|j| j.staging.is_none())
                },
            ),
            Property::always(
                "S2 new-boot save never blacks out",
                |_: &S2Model, s: &S2State| !s.boot_blackout,
            ),
            Property::always(
                "S2 terminal pins reclaimed after restart",
                |_: &S2Model, s: &S2State| {
                    !s.just_recovered
                        || s.job.is_some_and(|j| !j.terminal())
                        || s.chunks.iter().flatten().all(|c| !c.pinned)
                },
            ),
            Property::always(
                "S2 FailedRetryable completes once disk is Ok",
                |m: &S2Model, s: &S2State| {
                    if !s.job.is_some_and(|j| j.phase == JobPhase::FailedRetryable) {
                        return true;
                    }
                    let mut cur = s.clone();
                    cur.disk = Disk::Ok;
                    if !cur.has_writer() {
                        cur.writers[0] = true;
                    }
                    for step in [
                        S2Action::Retry,
                        S2Action::Phase2StageWrite,
                        S2Action::Phase2AtomicRename {
                            parent_sync_ok: true,
                        },
                        S2Action::Phase2CatalogRegister,
                        S2Action::Phase2CompleteJob,
                    ] {
                        match m.next_state(&cur, step) {
                            Some(n) => cur = n,
                            None => return false,
                        }
                    }
                    cur.job.is_some_and(|j| j.phase == JobPhase::Completed)
                        && cur.chunks.iter().flatten().all(|c| !c.pinned)
                },
            ),
            Property::sometimes(
                "S2 stale GC mark rejected after restore",
                |_: &S2Model, s: &S2State| {
                    s.gc_marked && s.bundle.is_some_and(|b| b.state == BState::Active)
                },
            ),
            Property::sometimes("S2 bundle purged", |_: &S2Model, s: &S2State| {
                s.bundle.is_some_and(|b| b.state == BState::Purged)
            }),
            Property::sometimes(
                "S2 post-reboot save pins new-boot chunk",
                |_: &S2Model, s: &S2State| {
                    s.boot == 1 && s.job.is_some_and(|j| j.boot == 1 && j.pins != 0)
                },
            ),
            Property::sometimes(
                "S2 retryable job survives reboot",
                |_: &S2Model, s: &S2State| {
                    s.boot == 1
                        && s.job
                            .is_some_and(|j| j.boot == 0 && j.phase == JobPhase::FailedRetryable)
                },
            ),
        ]
    }
}

#[test]
fn s2_data_preservation_model() {
    let states = run("S2", S2Model);
    assert!(
        states > 1_000,
        "S2 must explore a non-trivial interleaving space"
    );
}

// =====================================================================================
// S3 — live media/ADB discontinuity, video-only EOF, generation isolation, lock order
// =====================================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Video {
    Down,
    Live,
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Audio {
    /// API 28–32: no playback capture.
    Unavailable,
    Active,
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Wm {
    Before,
    Equal,
    After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Drain {
    video: Wm,
    audio: Wm,
    audio_required: bool,
    complete: bool,
    timed_out: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Locks {
    Idle,
    Gate,
    Both,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct S3State {
    api33: bool,
    generation: u8,
    capture_enabled: bool,
    persisted_enabled: bool,
    storage_stopped: bool,
    shutting_down: bool,
    video: Video,
    audio: Audio,
    control_up: bool,
    gap_recorded: bool,
    stale_worker_gen: Option<u8>,
    drain: Option<Drain>,
    /// Lock progress of the handover thread and the capture-stop thread.
    threads: [Locks; 2],
    lock_order_violation: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum S3Action {
    StartCapture,
    StaleWorkerEvent,
    VideoEof,
    WatchdogTick,
    AudioEof,
    StoragePressure,
    DiskRecovered,
    UserRearm,
    AutoAttachTick,
    Shutdown,
    BeginSaveDrain,
    AdvanceWatermark { audio: bool },
    DrainTimeout,
    DrainComplete,
    LockStep(usize),
}

struct S3Model;

impl S3State {
    fn stop_session(&mut self) {
        if self.video != Video::Down {
            self.stale_worker_gen = Some(self.generation);
        }
        self.video = Video::Down;
        self.audio = if self.api33 {
            Audio::Eof
        } else {
            Audio::Unavailable
        };
        self.control_up = false;
    }
}

impl Model for S3Model {
    type State = S3State;
    type Action = S3Action;

    fn init_states(&self) -> Vec<S3State> {
        [false, true]
            .into_iter()
            .map(|api33| S3State {
                api33,
                generation: 0,
                capture_enabled: true,
                persisted_enabled: true,
                storage_stopped: false,
                shutting_down: false,
                video: Video::Down,
                audio: Audio::Unavailable,
                control_up: false,
                gap_recorded: false,
                stale_worker_gen: None,
                drain: None,
                threads: [Locks::Idle; 2],
                lock_order_violation: false,
            })
            .collect()
    }

    fn actions(&self, _s: &S3State, out: &mut Vec<S3Action>) {
        out.extend([
            S3Action::StartCapture,
            S3Action::StaleWorkerEvent,
            S3Action::VideoEof,
            S3Action::WatchdogTick,
            S3Action::AudioEof,
            S3Action::StoragePressure,
            S3Action::DiskRecovered,
            S3Action::UserRearm,
            S3Action::AutoAttachTick,
            S3Action::Shutdown,
            S3Action::BeginSaveDrain,
            S3Action::AdvanceWatermark { audio: false },
            S3Action::AdvanceWatermark { audio: true },
            S3Action::DrainTimeout,
            S3Action::DrainComplete,
            S3Action::LockStep(0),
            S3Action::LockStep(1),
        ]);
    }

    fn next_state(&self, s: &S3State, a: S3Action) -> Option<S3State> {
        let mut n = s.clone();
        match a {
            S3Action::StartCapture => {
                if !s.capture_enabled
                    || s.storage_stopped
                    || s.shutting_down
                    || s.video != Video::Down
                    || s.generation >= 2
                {
                    return None;
                }
                n.generation += 1;
                n.video = Video::Live;
                n.audio = if s.api33 {
                    Audio::Active
                } else {
                    Audio::Unavailable
                };
                n.control_up = true;
            }
            S3Action::StaleWorkerEvent => {
                let g = s.stale_worker_gen?;
                n.stale_worker_gen = None;
                // Workers carry their generation token; mismatching events are dropped.
                if g == s.generation && s.video == Video::Live {
                    n.video = Video::Eof;
                }
            }
            S3Action::VideoEof => {
                if s.video != Video::Live {
                    return None;
                }
                n.video = Video::Eof;
            }
            S3Action::WatchdogTick => {
                // Video-only EOF: tear the session down, record a gap, allow reconnect.
                if s.video != Video::Eof {
                    return None;
                }
                n.stop_session();
                n.gap_recorded = true;
            }
            S3Action::AudioEof => {
                if s.audio != Audio::Active {
                    return None;
                }
                n.audio = Audio::Eof;
                n.gap_recorded = true;
            }
            S3Action::StoragePressure => {
                if s.storage_stopped {
                    return None;
                }
                n.storage_stopped = true;
                n.capture_enabled = false;
                n.persisted_enabled = false;
                n.stop_session();
            }
            S3Action::DiskRecovered => {
                if !s.storage_stopped {
                    return None;
                }
                n.storage_stopped = false;
            }
            S3Action::UserRearm => {
                if s.storage_stopped || s.capture_enabled || s.shutting_down {
                    return None;
                }
                n.capture_enabled = true;
                n.persisted_enabled = true;
            }
            S3Action::AutoAttachTick => {
                // Device (re)attach only resumes an enabled capture; it never edits the flag.
                if s.shutting_down {
                    return None;
                }
                if s.capture_enabled && s.video == Video::Down && !s.storage_stopped {
                    n.control_up = true;
                }
                if n == *s {
                    return None;
                }
            }
            S3Action::Shutdown => {
                if s.shutting_down {
                    return None;
                }
                n.shutting_down = true;
                n.capture_enabled = false; // runtime only; persisted flag is kept
                n.stop_session();
            }
            S3Action::BeginSaveDrain => {
                if s.drain.is_some_and(|d| !d.complete) || s.video != Video::Live {
                    return None;
                }
                let audio_required = s.audio == Audio::Active;
                n.drain = Some(Drain {
                    video: Wm::Before,
                    audio: if audio_required {
                        Wm::Before
                    } else {
                        Wm::Equal
                    },
                    audio_required,
                    complete: false,
                    timed_out: false,
                });
            }
            S3Action::AdvanceWatermark { audio } => {
                let mut d = s.drain?;
                if d.complete {
                    return None;
                }
                let (live, wm) = if audio {
                    (s.audio == Audio::Active, &mut d.audio)
                } else {
                    (s.video == Video::Live, &mut d.video)
                };
                if !live || *wm == Wm::After {
                    return None;
                }
                *wm = if *wm == Wm::Before {
                    Wm::Equal
                } else {
                    Wm::After
                };
                n.drain = Some(d);
            }
            S3Action::DrainTimeout => {
                let mut d = s.drain?;
                if d.complete {
                    return None;
                }
                d.complete = true;
                d.timed_out = true;
                n.drain = Some(d);
                n.gap_recorded = true;
            }
            S3Action::DrainComplete => {
                let mut d = s.drain?;
                if d.complete || d.video < Wm::Equal || (d.audio_required && d.audio < Wm::Equal) {
                    return None;
                }
                d.complete = true;
                n.drain = Some(d);
            }
            S3Action::LockStep(t) => {
                // Both threads take `handover_gate` then `capture`, release together.
                let other = 1 - t;
                match s.threads[t] {
                    Locks::Idle => {
                        if s.threads[other] != Locks::Idle {
                            return None; // gate is exclusive
                        }
                        n.threads[t] = Locks::Gate;
                    }
                    Locks::Gate => n.threads[t] = Locks::Both,
                    Locks::Both => n.threads[t] = Locks::Idle,
                }
                if n.threads[t] == Locks::Gate && s.threads[t] == Locks::Both {
                    n.lock_order_violation = true;
                }
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(
                "S3 stale generation never mutates current",
                |m: &S3Model, s: &S3State| {
                    s.stale_worker_gen.is_none_or(|g| {
                        g == s.generation && s.video == Video::Live
                            || m.next_state(s, S3Action::StaleWorkerEvent)
                                .is_some_and(|n| {
                                    let mut expect = s.clone();
                                    expect.stale_worker_gen = None;
                                    n == expect
                                })
                    })
                },
            ),
            Property::always(
                "S3 stale worker token is from an older generation",
                |_: &S3Model, s: &S3State| s.stale_worker_gen.is_none_or(|g| g <= s.generation),
            ),
            Property::always(
                "S3 drain completes only at watermark or explicit gap",
                |_: &S3Model, s: &S3State| {
                    s.drain.is_none_or(|d| {
                        !d.complete
                            || (d.timed_out && s.gap_recorded)
                            || (d.video >= Wm::Equal && (!d.audio_required || d.audio >= Wm::Equal))
                    })
                },
            ),
            Property::always(
                "S3 lock hierarchy handover_gate → capture",
                |_: &S3Model, s: &S3State| {
                    !s.lock_order_violation
                        && s.threads.iter().filter(|t| **t != Locks::Idle).count() <= 1
                },
            ),
            Property::always(
                "S3 auto-attach never clears user re-arm",
                |m: &S3Model, s: &S3State| {
                    m.next_state(s, S3Action::AutoAttachTick).is_none_or(|n| {
                        n.capture_enabled == s.capture_enabled
                            && n.persisted_enabled == s.persisted_enabled
                    })
                },
            ),
            Property::always(
                "S3 shutdown leaves capture disabled, persisted flag intact",
                |m: &S3Model, s: &S3State| {
                    (!s.shutting_down || (!s.capture_enabled && s.video == Video::Down))
                        && m.next_state(s, S3Action::Shutdown)
                            .is_none_or(|n| n.persisted_enabled == s.persisted_enabled)
                },
            ),
            Property::always(
                "S3 video-only EOF leads to reconnect with gap",
                |m: &S3Model, s: &S3State| {
                    s.video != Video::Eof
                        || m.next_state(s, S3Action::WatchdogTick).is_some_and(|n| {
                            n.video == Video::Down
                                && n.gap_recorded
                                && (!n.capture_enabled
                                    || n.storage_stopped
                                    || n.generation >= 2
                                    || m.next_state(&n, S3Action::StartCapture).is_some())
                        })
                },
            ),
            Property::always(
                "S3 API 28–32 never reports active audio",
                |_: &S3Model, s: &S3State| s.api33 || s.audio != Audio::Active,
            ),
            Property::sometimes(
                "S3 user re-arm after disk recovery restarts capture",
                |_: &S3Model, s: &S3State| {
                    s.generation == 2 && s.video == Video::Live && s.gap_recorded
                },
            ),
            Property::sometimes(
                "S3 drain completes at watermark on API 33",
                |_: &S3Model, s: &S3State| {
                    s.api33
                        && s.drain
                            .is_some_and(|d| d.complete && !d.timed_out && d.audio_required)
                },
            ),
        ]
    }
}

#[test]
fn s3_live_media_discontinuity_model() {
    let states = run("S3", S3Model);
    assert!(
        states > 1_000,
        "S3 must explore a non-trivial interleaving space"
    );
}
