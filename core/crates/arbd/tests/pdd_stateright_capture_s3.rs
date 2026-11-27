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
