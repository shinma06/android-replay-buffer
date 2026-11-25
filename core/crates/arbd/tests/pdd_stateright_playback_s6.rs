//! S6: one editor, its serialized seek RPC, playback clock and replacement viewport.
//! Generation numbers are quotiented by equality to the controller's current generation.
//! Only the newest queued seek can pass performExactSeek's generation check.

use serde::{Deserialize, Serialize};
use stateright::{Checker, HasDiscoveries, Model, Path, Property};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum Action {
    Play,
    Pause,
    AdvanceClock(u8),
    Elapse(u8),
    UserSeekTime(u8),
    StepFrame(i8),
    SeekRequestIssued(u8),
    SeekSuccess(u8),
    SeekTransientError,
    SeekPermanentError,
    ViewportNotFoundDisconnect,
    AcquireMissing,
    ReopenFailed,
    SharedRpcDisconnect,
    ConnectViewport,
    Reopen,
    Dispose,
    CloseSession,
    PrepareWait,
    CommitWait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Phase {
    Idle,
    Seeking,
    Connecting,
    SharedReconnect,
    Opening,
    Replaying,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Wait {
    Running,
    Prepared,
    Signalled,
    Parked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Clock {
    Running,
    Frozen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum Violation {
    #[default]
    None,
    Rewind,
    HoldSeek,
    SilentFailure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutation {
    None,
    StaleError,
    ArrivalRewinds,
    Leak,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
enum Selection {
    Full,
    Loop,
    SingleLoop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent observable controller and resource flags"
)]
struct State {
    playing: bool,
    t: u8,
    target: u8,
    displayed: u8,
    clock: Clock,
    queued: Option<u8>,
    flight: Option<u8>,
    stale: bool,
    retry: bool,
    phase: Phase,
    viewport: bool,
    reader: bool,
    disposed: bool,
    closed: bool,
    error: bool,
    wait: Wait,
    violation: Violation,
}

impl State {
    fn signal(&mut self) {
        self.wait = match self.wait {
            Wait::Prepared => Wait::Signalled,
            Wait::Parked => Wait::Running,
            other => other,
        };
    }

    fn invalidate(&mut self) {
        self.queued = None;
        self.stale = self.flight.is_some();
        self.retry = false;
        self.clock = Clock::Running;
        self.signal();
    }

    fn seek(&mut self, target: u8) {
        self.invalidate();
        self.target = target;
        self.queued = Some(target);
    }

    fn finish(&mut self) {
        self.flight = None;
        self.phase = Phase::Idle;
        if !self.stale {
            self.clock = Clock::Running;
            self.signal();
        }
        self.stale = false;
    }
}

struct S6 {
    selection: Selection,
    mutation: Mutation,
}

fn frame(t: u8) -> u8 {
    u8::from(t >= 3)
}

impl S6 {
    fn end(&self) -> u8 {
        if self.selection == Selection::SingleLoop {
            3
        } else {
            4
        }
    }

    fn fail(&self, s: &mut State) {
        if !s.disposed && (!s.stale || self.mutation == Mutation::StaleError) {
            s.playing = false;
            s.error = true;
            s.signal();
        }
        s.finish();
    }
}

impl Model for S6 {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            playing: false,
            t: 0,
            target: 0,
            displayed: 0,
            clock: Clock::Running,
            queued: None,
            flight: None,
            stale: false,
            retry: false,
            phase: Phase::Idle,
            viewport: true,
            reader: true,
            disposed: false,
            closed: false,
            error: false,
            wait: Wait::Running,
            violation: Violation::None,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        use Action::*;
        if !s.disposed {
            out.extend([
                Play,
                Pause,
                Dispose,
                PrepareWait,
                CommitWait,
                StepFrame(-1),
                StepFrame(1),
            ]);
            out.extend((0..=4).map(UserSeekTime));
            for dt in [1, 3, 4] {
                out.extend([AdvanceClock(dt), Elapse(dt)]);
            }
        }
        match s.phase {
            Phase::Idle => {
                if let Some(target) = s.queued {
                    out.push(if s.viewport {
                        SeekRequestIssued(target)
                    } else {
                        AcquireMissing
                    });
                }
                if s.disposed && !s.closed {
                    out.push(CloseSession);
                }
            }
            Phase::Connecting => out.extend([SharedRpcDisconnect, ConnectViewport]),
            Phase::SharedReconnect => out.push(ConnectViewport),
            Phase::Opening => out.extend([Reopen, ReopenFailed]),
            Phase::Seeking | Phase::Replaying => {
                out.extend([
                    SeekSuccess(0),
                    SeekSuccess(1),
                    SeekPermanentError,
                    SeekTransientError,
                    ViewportNotFoundDisconnect,
                ]);
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "each arm is a linearization point checked also by fixture replay"
    )]
    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        use Action::*;
        let mut n = *s;
        match a {
            Play if !s.playing => {
                n.invalidate();
                if n.t >= self.end() {
                    n.t = 0;
                }
                n.playing = true;
                n.error = false;
                let target = frame(n.t);
                if target != s.target || target != s.displayed {
                    n.seek(target);
                    n.clock = Clock::Frozen;
                }
            }
            Pause if s.playing => {
                n.invalidate();
                n.playing = false;
            }
            UserSeekTime(t) => {
                n.t = if s.playing { t.min(self.end()) } else { t };
                n.seek(frame(t));
            }
            StepFrame(delta) => {
                let target = u8::from(delta >= 0);
                n.playing = false;
                n.t = target * 3;
                n.seek(target);
            }
            AdvanceClock(dt) | Elapse(dt) if s.playing && s.clock == Clock::Running => {
                n.t = (s.t + dt).min(self.end());
                if matches!(a, AdvanceClock(_)) {
                    if n.t == self.end() {
                        match self.selection {
                            Selection::Full => {
                                n.playing = false;
                                if s.target != 1 {
                                    n.seek(1);
                                }
                            }
                            Selection::Loop | Selection::SingleLoop => {
                                n.t = 0;
                                if self.selection != Selection::SingleLoop || s.target != 0 {
                                    n.seek(0);
                                }
                            }
                        }
                    } else if frame(n.t) > s.target {
                        n.seek(frame(n.t));
                    }
                    let holding = (s.t + dt < self.end() && s.target == frame(n.t))
                        || (self.selection == Selection::SingleLoop && s.target == 0);
                    if holding && n.queued != s.queued {
                        n.violation = Violation::HoldSeek;
                    }
                }
            }
            Elapse(_) => {} // A frozen or paused controller ignores wall time.
            SeekRequestIssued(target) if s.queued == Some(target) && s.flight.is_none() => {
                n.flight = n.queued.take();
                n.phase = Phase::Seeking;
            }
            SeekSuccess(displayed) => {
                if !s.stale {
                    n.displayed = displayed;
                    if self.mutation == Mutation::ArrivalRewinds {
                        n.t = displayed * 3;
                    }
                }
                n.finish();
            }
            SeekTransientError if !s.retry && !s.stale => {
                n.finish();
                n.queued = s.flight;
                n.retry = true;
                if s.playing {
                    n.clock = Clock::Frozen;
                }
            }
            ViewportNotFoundDisconnect if s.phase == Phase::Seeking && !s.disposed => {
                n.phase = Phase::Connecting;
                n.viewport = false;
                n.reader = false;
                // arbd reclaims the old viewport at socket EOF.
                if !s.stale && s.playing {
                    n.clock = Clock::Frozen;
                }
            }
            ViewportNotFoundDisconnect => {
                n.viewport = false;
                n.reader = false;
                self.fail(&mut n);
            }
            SeekTransientError | SeekPermanentError => {
                self.fail(&mut n);
            }
            AcquireMissing => {
                n.flight = n.queued.take();
                n.phase = Phase::Connecting;
                if s.playing {
                    n.clock = Clock::Frozen;
                }
            }
            ReopenFailed => {
                n.viewport = false;
                self.fail(&mut n);
            }
            SharedRpcDisconnect => n.phase = Phase::SharedReconnect,
            ConnectViewport => {
                // The daemon allocated the replacement before Studio received the descriptor.
                n.viewport = true;
                n.phase = Phase::Opening;
            }
            Reopen if s.disposed => {
                if self.mutation != Mutation::Leak {
                    n.viewport = false;
                }
                n.finish();
            }
            Reopen => {
                n.reader = true;
                n.phase = Phase::Replaying;
            }
            Dispose => {
                n.invalidate();
                n.playing = false;
                n.disposed = true;
            }
            CloseSession => {
                n.closed = true;
                if n.reader {
                    n.viewport = false;
                    n.reader = false;
                }
            }
            PrepareWait if s.wait == Wait::Running => n.wait = Wait::Prepared,
            CommitWait if matches!(s.wait, Wait::Prepared | Wait::Signalled) => {
                n.wait = if !s.playing || s.clock == Clock::Frozen {
                    Wait::Parked
                } else {
                    Wait::Running
                };
            }
            _ => return None,
        }
        if matches!(
            a,
            SeekSuccess(_)
                | SeekPermanentError
                | SeekTransientError
                | ViewportNotFoundDisconnect
                | ReopenFailed
                | Reopen
        ) && (n.t != s.t
            || (s.stale
                && (s.playing != n.playing || s.error != n.error || s.displayed != n.displayed)))
        {
            n.violation = Violation::Rewind;
        }
        if !s.stale
            && (a == SeekPermanentError
                || a == ReopenFailed
                || (a == SeekTransientError && s.retry)
                || (a == ViewportNotFoundDisconnect && s.phase == Phase::Replaying))
            && (n.playing || !n.error)
        {
            n.violation = Violation::SilentFailure;
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("no spurious rewind", |_, s: &State| {
                s.violation != Violation::Rewind
            }),
            Property::always("hold zero seek", |_, s: &State| {
                s.violation != Violation::HoldSeek
            }),
            Property::always("bounded visible failure", |_, s: &State| {
                s.violation != Violation::SilentFailure
            }),
            Property::always(
                "one reopen restores the request or visibly stops",
                |m: &Self, s: &State| {
                    if s.phase != Phase::Opening || s.disposed {
                        return true;
                    }
                    let opened = m
                        .next_state(s, Action::Reopen)
                        .expect("descriptor delivered");
                    let frame = s.flight.expect("recovering an outstanding seek");
                    let success = m
                        .next_state(&opened, Action::SeekSuccess(frame))
                        .expect("retry completes");
                    let failure = m
                        .next_state(&opened, Action::ViewportNotFoundDisconnect)
                        .expect("second disconnect");
                    opened.reader
                        && opened.viewport
                        && opened.phase == Phase::Replaying
                        && success.flight.is_none()
                        && (s.stale || success.displayed == frame)
                        && failure.phase == Phase::Idle
                        && (s.stale || (!failure.playing && failure.error))
                },
            ),
            Property::always("at most one controller retry", |m: &Self, s: &State| {
                if !matches!(s.phase, Phase::Seeking | Phase::Replaying) || s.stale || s.retry {
                    return true;
                }
                let first = m
                    .next_state(s, Action::SeekTransientError)
                    .expect("first failure");
                let issued = m.next_state(
                    &first,
                    Action::SeekRequestIssued(s.flight.expect("outstanding seek")),
                );
                issued
                    .and_then(|retry| m.next_state(&retry, Action::SeekTransientError))
                    .is_some_and(|end| {
                        !end.playing && end.error && end.flight.is_none() && end.queued.is_none()
                    })
            }),
            Property::always("no lost wakeup", |_, s: &State| {
                !s.playing || s.clock == Clock::Frozen || s.wait != Wait::Parked
            }),
            Property::always("dispose drains all ownership", |m: &Self, s: &State| {
                if !s.disposed {
                    return true;
                }
                let mut n = *s;
                if matches!(n.phase, Phase::Connecting | Phase::SharedReconnect) {
                    n = m
                        .next_state(&n, Action::ConnectViewport)
                        .expect("verified socket");
                }
                if n.phase == Phase::Opening {
                    n = m.next_state(&n, Action::Reopen).expect("open returns");
                }
                if n.flight.is_some() {
                    n = m
                        .next_state(&n, Action::SeekSuccess(0))
                        .expect("RPC returns");
                }
                if !n.closed {
                    n = m
                        .next_state(&n, Action::CloseSession)
                        .expect("close obtains monitor");
                }
                !n.viewport && !n.reader
            }),
        ]
    }
}

#[test]
fn exhaustive_s6() {
    let start = Instant::now();
    for selection in [Selection::Full, Selection::Loop, Selection::SingleLoop] {
        let checker = S6 {
            selection,
            mutation: Mutation::None,
        }
        .checker()
        .spawn_bfs()
        .join();
        checker.assert_properties();
        println!("{selection:?}: {} states", checker.unique_state_count());
    }
    println!("elapsed {:?}", start.elapsed());
}

#[test]
fn negative_controls() {
    for (mutation, property) in [
        (Mutation::ArrivalRewinds, "no spurious rewind"),
        (Mutation::StaleError, "no spurious rewind"),
        (Mutation::Leak, "dispose drains all ownership"),
    ] {
        let checker = S6 {
            selection: Selection::Full,
            mutation,
        }
        .checker()
        .finish_when(HasDiscoveries::AnyFailures)
        .spawn_bfs()
        .join();
        let discovery = checker.assert_any_discovery(property);
        let broken = checker
            .model()
            .properties()
            .into_iter()
            .find(|p| p.name == property)
            .expect("mutation property");
        assert!(
            !(broken.condition)(checker.model(), discovery.last_state()),
            "the reported path must violate {property}"
        );
        let path = discovery.into_actions();
        assert!(!path.is_empty(), "expected a reachable faulty transition");
        println!("{mutation:?}: {path:?}");
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Observation {
    mode: String,
    playhead_time_ns: u8,
    displayed_frame: u8,
    error_visible: bool,
    seek_count: usize,
    open_viewports_after_dispose: u8,
}

#[derive(Deserialize)]
struct Trace {
    name: String,
    selection: Selection,
    events: Vec<Action>,
    expected: Observation,
}

// In a replay the environment fixes the order of UI inputs and injected replies.
// The checker supplies the controller's RPC actions (including their frame numbers)
// from S6. A trace barrier waits for a runnable seek before feeding the next input.
struct ReplaySchedule {
    model: S6,
    inputs: Vec<Action>,
}

impl Model for ReplaySchedule {
    type State = (State, usize);
    type Action = Action;

    fn init_states(&self) -> Vec<Self::State> {
        vec![(self.model.init_states()[0], 0)]
    }

    fn actions(&self, (state, cursor): &Self::State, out: &mut Vec<Action>) {
        self.model.actions(state, out);
        if out
            .iter()
            .any(|action| matches!(action, Action::SeekRequestIssued(_)))
        {
            out.retain(|action| matches!(action, Action::SeekRequestIssued(_)));
        } else {
            out.retain(|action| Some(action) == self.inputs.get(*cursor));
        }
    }

    fn next_state(&self, (state, cursor): &Self::State, action: Action) -> Option<Self::State> {
        let consumed = usize::from(!matches!(action, Action::SeekRequestIssued(_)));
        self.model
            .next_state(state, action)
            .map(|next| (next, cursor + consumed))
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![Property::<Self>::sometimes(
            "settled replay",
            |schedule, (state, cursor)| {
                *cursor == schedule.inputs.len() && state.flight.is_none() && state.queued.is_none()
            },
        )]
    }
}

#[test]
fn shared_traces() {
    let traces: Vec<Trace> = serde_json::from_str(include_str!(
        "../../../../docs/specs/playback-s6-traces.json"
    ))
    .expect("S6 fixture");
    for trace in traces {
        let model = S6 {
            selection: trace.selection,
            mutation: Mutation::None,
        };
        let schedule = ReplaySchedule {
            model: S6 {
                selection: trace.selection,
                mutation: Mutation::None,
            },
            inputs: trace
                .events
                .iter()
                .copied()
                .filter(|event| !matches!(event, Action::SeekRequestIssued(_)))
                .collect(),
        };
        let checker = schedule.checker().spawn_bfs().join();
        let generated = checker
            .assert_any_discovery("settled replay")
            .into_actions();
        assert_eq!(generated, trace.events, "RPC schedule for {}", trace.name);
        let path = Path::from_actions(&model, model.init_states()[0], &trace.events)
            .unwrap_or_else(|| panic!("unreachable trace {}", trace.name));
        let s = *path.last_state();
        assert!(
            s.flight.is_none() && s.queued.is_none(),
            "unfinished {}",
            trace.name
        );
        let seek_count = path
            .into_vec()
            .iter()
            .filter(|(before, a)| {
                matches!(a, Some(Action::SeekRequestIssued(_)))
                    || (a == &Some(Action::Reopen) && !before.disposed)
            })
            .count();
        let mut closed = s;
        if !closed.disposed {
            closed = model.next_state(&closed, Action::Dispose).expect("dispose");
        }
        if !closed.closed {
            closed = model
                .next_state(&closed, Action::CloseSession)
                .expect("close");
        }
        let observed = Observation {
            mode: if s.playing { "PLAYING" } else { "PAUSED" }.into(),
            playhead_time_ns: s.t,
            displayed_frame: s.displayed,
            error_visible: s.error,
            seek_count,
            open_viewports_after_dispose: u8::from(closed.viewport),
        };
        assert_eq!(observed, trace.expected, "{}", trace.name);
    }
}
