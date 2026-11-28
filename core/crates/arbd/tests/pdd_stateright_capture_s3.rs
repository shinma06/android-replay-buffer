//! S3 capture intent, restart and delayed device attachment.

use stateright::{Checker, Model, Property};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct AttachToken {
    serial: u8,
    generation: u8,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct AttachState {
    selected: AttachToken,
    persisted_serial: u8,
    pending: Option<AttachToken>,
    live: Option<AttachToken>,
    pressure: bool,
    armed: bool,
    rearmed: bool,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum AttachAction {
    Begin,
    Finish,
    Select(u8),
    Pressure,
    Recover,
    Rearm(Option<u8>),
    Restart,
}
struct AttachModel;
impl Model for AttachModel {
    type State = AttachState;
    type Action = AttachAction;
    fn init_states(&self) -> Vec<Self::State> {
        vec![AttachState {
            selected: AttachToken {
                serial: 0,
                generation: 0,
            },
            persisted_serial: 0,
            pending: None,
            live: None,
            pressure: false,
            armed: true,
            rearmed: false,
        }]
    }
    fn actions(&self, _: &Self::State, out: &mut Vec<Self::Action>) {
        use AttachAction::*;
        out.extend([
            Begin,
            Finish,
            Select(0),
            Select(1),
            Pressure,
            Recover,
            Rearm(None),
            Rearm(Some(0)),
            Rearm(Some(1)),
            Restart,
        ]);
    }
    fn next_state(&self, s: &Self::State, a: Self::Action) -> Option<Self::State> {
        use AttachAction::*;
        let mut n = *s;
        match a {
            Begin if s.armed && !s.pressure && s.pending.is_none() && s.live.is_none() => {
                n.pending = Some(s.selected);
            }
            Finish => {
                let token = n.pending.take()?;
                if s.armed && !s.pressure && token == s.selected {
                    n.live = Some(token);
                }
            }
            Select(serial) if s.selected.generation < 2 => {
                n.persisted_serial = serial;
                n.selected = AttachToken {
                    serial,
                    generation: s.selected.generation + 1,
                };
                n.live = None;
            }
            Pressure if !s.pressure && s.selected.generation < 2 => {
                n.pressure = true;
                n.armed = false;
                n.live = None;
                n.rearmed = false;
                n.selected.generation += 1;
            }
            Recover if s.pressure => n.pressure = false,
            Restart => {
                n.live = None;
                n.pending = None;
                n.selected.serial = s.persisted_serial;
            }
            Rearm(serial) if !s.pressure && !s.armed => {
                if let Some(serial) = serial {
                    n.persisted_serial = serial;
                    n.selected.serial = serial;
                }
                n.armed = true;
                n.rearmed = true;
            }
            _ => return None,
        }
        (n != *s).then_some(n)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always(
                "S3 only selected serial and current generation live",
                |_, s| {
                    s.live
                        .is_none_or(|t| t == s.selected && s.armed && !s.pressure)
                },
            ),
            Property::<Self>::always("S3 selected device survives daemon restart", |m, s| {
                s.selected.serial == s.persisted_serial
                    && m.next_state(s, AttachAction::Restart).is_none_or(|n| {
                        n.selected.serial == s.selected.serial && n.armed == s.armed
                    })
            }),
            Property::<Self>::sometimes("S3 explicitly rearmed on second device", |_, s| {
                s.rearmed && s.live.is_some_and(|token| token.serial == 1)
            }),
            Property::<Self>::always("S3 rearm persists through late attach", |_, s| {
                !s.rearmed || s.armed
            }),
            Property::<Self>::sometimes("S3 stale attach on other serial", |_, s| {
                s.pending.is_some_and(|t| t.serial != s.selected.serial)
            }),
            Property::<Self>::sometimes("S3 capture resumes after pressure", |_, s| {
                s.rearmed && s.live.is_some()
            }),
        ]
    }
}
#[test]
fn s3_delayed_attach_model() {
    let checker = AttachModel.checker().spawn_bfs().join();
    println!("S3 attach: {} unique states", checker.unique_state_count());
    checker.assert_properties();
}

// ---------------------------------------------------------------------------------------------
// S3b (#583): two-phase explicit start, reservation exclusion and confirmed-pin re-attach.
//
// Serials: 0 = A, 1 = B (both pluggable), 2 = a typo that is never online. The model mirrors
// `handle_capture_start`: a start without serial persists the armed intent up front; an explicit
// serial is tentative until the attach finds it `Selected` and starts a session, and only then
// commits `(enabled=true, serial)` in one write. A start never takes over a held reservation.
// ---------------------------------------------------------------------------------------------

const TYPO: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Persisted {
    enabled: bool,
    serial: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Reservation {
    /// Auto-attach discovery for the persisted preference.
    Auto,
    /// A `capture.start` attach; `Some` is an explicit, still tentative serial.
    Start(Option<u8>),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Reply {
    Started,
    AlreadyActive,
    Waiting,
    Conflict,
    NotFound,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct StartState {
    online: [bool; 2],
    persisted: Persisted,
    live: Option<u8>,
    reservation: Option<Reservation>,
    /// Snapshot taken when the pending start was admitted.
    admitted: Option<(Persisted, Option<u8>)>,
    reply: Option<Reply>,
    /// Ghost: a failed start changed `persisted` or the live session.
    failure_mutated: bool,
    /// Ghost: an auto-attach reservation vanished without its own outcome, stop or restart.
    auto_lost: bool,
    /// Ghost: serials that were observed `Selected` by an explicit start.
    selected_seen: [bool; 3],
    /// Ghost: the explicit pin was confirmed, then unplugged, replugged and re-attached.
    reattach: u8,
    /// A second start queued behind a held reservation; `true` once a stop overtook it.
    queued: Option<(Option<u8>, bool)>,
    /// Ghost: a queued start was admitted although a stop arrived while it waited.
    resumed_after_stop: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum StartAction {
    Plug(u8),
    Unplug(u8),
    Start(Option<u8>),
    AttachDone,
    AutoBegin,
    Stop,
    Restart,
    Ack,
    /// The queued start polls the reservation again (`StartAdmission::Busy` loop).
    Retry,
}

/// `admit_capture_start`: `false` means `Busy` (a reservation is held and nothing changed).
fn admit(s: &StartState, n: &mut StartState, arg: Option<u8>) -> bool {
    if let Some(active) = s.live {
        // Recording: the same serial (or none) is idempotent, another is CONFLICT.
        if arg.is_none_or(|serial| serial == active) {
            n.persisted = Persisted {
                enabled: true,
                serial: arg.or(s.persisted.serial),
            };
            if let Some(serial) = arg {
                // The running session is the observation that this serial is Selected.
                n.selected_seen[usize::from(serial)] = true;
            }
            n.reply = Some(Reply::AlreadyActive);
        } else {
            n.reply = Some(Reply::Conflict);
            n.failure_mutated |= n.persisted != s.persisted || n.live != s.live;
        }
        return true;
    }
    if s.reservation.is_some() {
        return false;
    }
    if arg.is_none() {
        n.persisted.enabled = true;
    }
    n.reservation = Some(Reservation::Start(arg));
    n.admitted = Some((n.persisted, n.live));
    true
}

fn is_online(s: &StartState, serial: u8) -> bool {
    serial < 2 && s.online[usize::from(serial)]
}

/// `DeviceSelectionPolicy::evaluate`: a pin never switches; without one only a sole device.
fn select(s: &StartState, preferred: Option<u8>) -> Option<u8> {
    match preferred {
        Some(serial) => is_online(s, serial).then_some(serial),
        None => match s.online {
            [true, false] => Some(0),
            [false, true] => Some(1),
            _ => None,
        },
    }
}

struct StartModel;
impl Model for StartModel {
    type State = StartState;
    type Action = StartAction;
    fn init_states(&self) -> Vec<Self::State> {
        let base = StartState {
            online: [true, false],
            persisted: Persisted {
                enabled: false,
                serial: None,
            },
            live: None,
            reservation: None,
            admitted: None,
            reply: None,
            failure_mutated: false,
            auto_lost: false,
            selected_seen: [false; 3],
            reattach: 0,
            queued: None,
            resumed_after_stop: false,
        };
        vec![
            base,
            StartState {
                online: [true, true],
                ..base
            },
        ]
    }
    fn actions(&self, _: &Self::State, out: &mut Vec<Self::Action>) {
        use StartAction::*;
        out.extend([
            Plug(0),
            Plug(1),
            Unplug(0),
            Unplug(1),
            Start(None),
            Start(Some(0)),
            Start(Some(1)),
            Start(Some(TYPO)),
            AttachDone,
            AutoBegin,
            Stop,
            Restart,
            Ack,
            Retry,
        ]);
    }
    #[expect(
        clippy::too_many_lines,
        reason = "one match arm per modelled RPC transition"
    )]
    fn next_state(&self, s: &Self::State, a: Self::Action) -> Option<Self::State> {
        use StartAction::*;
        let mut n = *s;
        match a {
            Plug(d) if !s.online[usize::from(d)] => {
                n.online[usize::from(d)] = true;
                if s.reattach == 2 && Some(d) == s.persisted.serial {
                    n.reattach = 3;
                }
            }
            Unplug(d) if s.online[usize::from(d)] => {
                n.online[usize::from(d)] = false;
                // The session's supervisor gives up; auto-attach resumes from the persisted pin.
                if s.live == Some(d) {
                    n.live = None;
                }
                if s.reattach == 1 && Some(d) == s.persisted.serial {
                    n.reattach = 2;
                }
            }
            Start(arg) if s.reply.is_none() && s.admitted.is_none() && s.queued.is_none() => {
                if !admit(s, &mut n, arg) {
                    // Queue behind the holder (`StartAdmission::Busy`), never take it over.
                    n.queued = Some((arg, false));
                }
            }
            AttachDone => match s.reservation? {
                Reservation::Auto => {
                    n.reservation = None;
                    if s.persisted.enabled
                        && let Some(serial) = select(s, s.persisted.serial)
                    {
                        n.live = Some(serial);
                    }
                }
                Reservation::Start(arg) => {
                    n.reservation = None;
                    n.admitted = None;
                    match (arg, select(s, arg.or(s.persisted.serial))) {
                        (Some(serial), Some(selected)) => {
                            // Selected and started: commit both fields in one write.
                            n.live = Some(selected);
                            n.persisted = Persisted {
                                enabled: true,
                                serial: Some(serial),
                            };
                            n.selected_seen[usize::from(serial)] = true;
                            n.reply = Some(Reply::Started);
                            if s.reattach == 0 {
                                n.reattach = 1;
                            }
                        }
                        (Some(_), None) => {
                            n.reply = Some(Reply::NotFound);
                            let (persisted, _) = s.admitted?;
                            n.failure_mutated |= n.persisted != persisted
                                || n.persisted != s.persisted
                                || n.live != s.live;
                        }
                        (None, Some(selected)) => {
                            n.live = Some(selected);
                            n.reply = Some(Reply::Started);
                        }
                        (None, None) => n.reply = Some(Reply::Waiting),
                    }
                }
            },
            AutoBegin if s.persisted.enabled && s.reservation.is_none() && s.live.is_none() => {
                n.reservation = Some(Reservation::Auto);
            }
            Stop if s.persisted.enabled || s.live.is_some() || s.reservation.is_some() => {
                n.persisted.enabled = false;
                n.live = None;
                n.reservation = None;
                n.admitted = None;
                // `stop_epoch` moves on: a start that entered earlier is cancelled.
                n.queued = s.queued.map(|(arg, _)| (arg, true));
            }
            Retry if s.admitted.is_none() && s.reply.is_none() => {
                let (arg, stale) = s.queued?;
                if stale {
                    // `stop_epoch` changed while queued: cancelled, nothing admitted.
                    n.queued = None;
                    n.reply = Some(Reply::Conflict);
                } else if admit(s, &mut n, arg) {
                    n.queued = None;
                    n.resumed_after_stop |= stale;
                } else {
                    return None;
                }
            }
            Restart => {
                n.live = None;
                n.reservation = None;
                n.admitted = None;
                n.reply = None;
                n.queued = None;
            }
            Ack if s.reply.is_some() => n.reply = None,
            _ => return None,
        }
        if s.reservation == Some(Reservation::Auto)
            && n.reservation != Some(Reservation::Auto)
            && !matches!(a, AttachDone | Stop | Restart)
        {
            n.auto_lost = true;
        }
        (n != *s).then_some(n)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always(
                "S3b failed explicit start leaves persisted intent and session unchanged",
                |_, s| !s.failure_mutated,
            ),
            Property::<Self>::always(
                "S3b persisted device_serial was observed Selected by an explicit start",
                |_, s| {
                    s.persisted
                        .serial
                        .is_none_or(|serial| s.selected_seen[usize::from(serial)])
                },
            ),
            Property::<Self>::always("S3b a typo serial is never persisted", |_, s| {
                s.persisted.serial != Some(TYPO)
            }),
            Property::<Self>::always(
                "S3b a start never silently drops an auto-attach reservation",
                |_, s| !s.auto_lost,
            ),
            Property::<Self>::always(
                "S3b a stop cancels starts queued before it (no resume after stop)",
                |_, s| !s.resumed_after_stop,
            ),
            Property::<Self>::sometimes("S3b a queued start is cancelled by stop", |_, s| {
                s.queued.is_some_and(|(_, stale)| stale)
            }),
            Property::<Self>::always(
                "S3b auto-attach never switches away from the pin (DS-07)",
                |_, s| {
                    s.live
                        .is_none_or(|live| s.persisted.serial.is_none_or(|pin| pin == live))
                },
            ),
            Property::<Self>::sometimes("S3b typo start is rejected", |_, s| {
                s.reply == Some(Reply::NotFound)
            }),
            Property::<Self>::sometimes("S3b other serial while recording conflicts", |_, s| {
                s.reply == Some(Reply::Conflict) && s.live.is_some()
            }),
            Property::<Self>::sometimes("S3b same serial while recording is idempotent", |_, s| {
                s.reply == Some(Reply::AlreadyActive)
            }),
            Property::<Self>::sometimes(
                "S3b confirmed pin re-attaches after unplug and replug",
                |_, s| s.reattach == 3 && s.live == s.persisted.serial && s.live.is_some(),
            ),
            Property::<Self>::sometimes(
                "S3b typo rejected, then argument-less start records",
                |_, s| {
                    s.persisted.serial.is_none()
                        && s.live == Some(0)
                        && s.reply == Some(Reply::Started)
                },
            ),
        ]
    }
}

#[test]
fn s3b_two_phase_explicit_start_model() {
    let checker = StartModel.checker().spawn_bfs().join();
    println!("S3b start: {} unique states", checker.unique_state_count());
    checker.assert_properties();
}
