//! S7. A single-pointer Studio timeline, presentation clock, and latest-wins preview decoder.
//! The exhaustive axis has a held sample, an interior instant, a sample boundary and EOF.
//! The same transitions generate the nanosecond observations on the shared 32-second VFR axis.

use serde::{Deserialize, Serialize};
use stateright::{Checker, HasDiscoveries, Model, Property};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Action {
    DragPlayheadStart(i64),
    DragPlayheadMove(i64),
    DragPlayheadEnd,
    DragInStart(i64),
    DragInMove(i64),
    DragInEnd,
    DragInCancel,
    DragOutStart(i64),
    DragOutMove(i64),
    DragOutEnd,
    DragOutCancel,
    NudgeIn(i8),
    NudgeOut(i8),
    FrameIn(i8),
    FrameOut(i8),
    MarkInFromPlayhead,
    MarkOutFromPlayhead,
    TogglePlay,
    PlayTick(i64),
    PlaybackStopsAt(i64),
    SampleStoppedClock,
    DecodeStart,
    LateFrameArrives,
    ZoomChange(u8, i64),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Target {
    None,
    Playhead,
    In,
    Out,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mutation {
    None,
    Coupled,
    Snap,
    Leaked,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Violation {
    None,
    Coupled,
    Snapped,
    Contaminated,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct State {
    playhead_ns: i64,
    in_ns: i64,
    out_ns: i64,
    playing: bool,
    stopped_clock: Option<i64>,
    drag_target: Target,
    original: i64,
    preview_time_ns: Option<i64>,
    zoom: u8,
    scroll: i64,
    pending_preview_frame: Option<usize>,
    // A generation is represented by equality to the latest request, even across A→B→A.
    late_frame_in_flight: Option<bool>,
    preview_displayed: bool,
    violation: Violation,
}

struct S7 {
    end: i64,
    minimum: i64,
    step: i64,
    starts: &'static [i64],
    times: &'static [i64],
    views: &'static [(u8, i64)],
    mutation: Mutation,
}

impl S7 {
    fn bounded(mutation: Mutation) -> Self {
        Self {
            end: 3,
            minimum: 1,
            step: 1,
            starts: &[0, 2],
            times: &[0, 1, 2, 3],
            views: &[(1, 0), (2, 1)],
            mutation,
        }
    }
    fn recording() -> Self {
        Self {
            end: 32_000_000_000,
            minimum: 1_000_000,
            step: 100_000_000,
            starts: &[0, 12_000_000_000, 22_000_000_000, 28_000_000_000],
            times: &[
                0,
                1_000_000,
                12_000_000_000,
                15_500_000_000,
                18_000_000_000,
                22_000_000_000,
                32_000_000_000,
            ],
            views: &[
                (1, 0),
                (2, 8_000_000_000),
                (4, 12_000_000_000),
                (8, 14_000_000_000),
            ],
            mutation: Mutation::None,
        }
    }
    fn frame(&self, time: i64) -> usize {
        self.starts.partition_point(|&t| t <= time.max(0)) - 1
    }
    fn boundary(&self, time: i64, direction: i8) -> i64 {
        if direction < 0 {
            self.starts[self.frame(time - 1)]
        } else {
            self.starts
                .get(self.frame(time) + 1)
                .copied()
                .unwrap_or(self.end)
        }
    }
    fn set_edge(&self, s: &mut State, edge: Target, time: i64) -> i64 {
        if edge == Target::In {
            s.in_ns = time.clamp(0, s.out_ns - self.minimum);
            s.in_ns
        } else {
            s.out_ns = time.clamp(s.in_ns + self.minimum, self.end);
            s.out_ns
        }
    }
    fn drag(&self, s: &mut State, edge: Target, t: i64, action: Action) -> Option<()> {
        if matches!(action, Action::DragInStart(_) | Action::DragOutStart(_)) {
            if s.drag_target != Target::None {
                return None;
            }
            s.original = if edge == Target::In {
                s.in_ns
            } else {
                s.out_ns
            };
        } else if s.drag_target != edge {
            return None;
        }
        s.drag_target = edge;
        let time = self.set_edge(s, edge, t);
        let frame = |time| self.frame(time - i64::from(edge == Target::Out));
        if s.preview_time_ns.map(frame) != Some(frame(time)) {
            Self::invalidate(s);
            s.pending_preview_frame = Some(frame(time));
        }
        s.preview_time_ns = Some(time);
        if self.mutation == Mutation::Coupled {
            s.playhead_ns = time;
        }
        Some(())
    }
    fn check_transition(&self, before: &State, action: Action, s: &mut State) {
        use Action::*;
        // Monitor pre/post observations independently from the transition implementation.
        let transport = matches!(
            action,
            DragPlayheadStart(_)
                | DragPlayheadMove(_)
                | DragPlayheadEnd
                | PlayTick(_)
                | PlaybackStopsAt(_)
                | SampleStoppedClock
        );
        if !transport && s.playhead_ns != before.playhead_ns {
            s.violation = Violation::Coupled;
        }
        if (transport || matches!(action, LateFrameArrives))
            && (s.in_ns, s.out_ns) != (before.in_ns, before.out_ns)
        {
            s.violation = Violation::Contaminated;
        }
        if (action == DragInEnd && Some(s.in_ns) != before.preview_time_ns)
            || (action == DragOutEnd && Some(s.out_ns) != before.preview_time_ns)
        {
            s.violation = Violation::Snapped;
        }
        match action {
            DragInStart(t) | DragInMove(t)
                if s.in_ns != t.clamp(0, before.out_ns - self.minimum) =>
            {
                s.violation = Violation::Snapped;
            }
            DragOutStart(t) | DragOutMove(t)
                if s.out_ns != t.clamp(before.in_ns + self.minimum, self.end) =>
            {
                s.violation = Violation::Snapped;
            }
            _ => {}
        }
    }
    fn invalidate(s: &mut State) {
        s.pending_preview_frame = None;
        if let Some(active) = &mut s.late_frame_in_flight {
            *active = false;
        }
        s.preview_displayed = false;
    }
}

impl Model for S7 {
    type State = State;
    type Action = Action;
    fn init_states(&self) -> Vec<State> {
        vec![State {
            playhead_ns: 0,
            in_ns: 0,
            out_ns: self.end,
            playing: false,
            stopped_clock: None,
            drag_target: Target::None,
            original: 0,
            preview_time_ns: None,
            zoom: 1,
            scroll: 0,
            pending_preview_frame: None,
            late_frame_in_flight: None,
            preview_displayed: false,
            violation: Violation::None,
        }]
    }
    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        use Action::*;
        for &t in self.times {
            match s.drag_target {
                Target::None => out.extend([DragPlayheadStart(t), DragInStart(t), DragOutStart(t)]),
                Target::Playhead => out.push(DragPlayheadMove(t)),
                Target::In => out.push(DragInMove(t)),
                Target::Out => out.push(DragOutMove(t)),
            }
            if s.playing && t >= s.playhead_ns {
                out.extend([PlayTick(t), PlaybackStopsAt(t)]);
            }
        }
        match s.drag_target {
            Target::None => {
                out.extend([MarkInFromPlayhead, MarkOutFromPlayhead, TogglePlay]);
                for d in [-1, 1] {
                    out.extend([NudgeIn(d), NudgeOut(d), FrameIn(d), FrameOut(d)]);
                }
            }
            Target::Playhead => out.push(DragPlayheadEnd),
            Target::In => out.extend([DragInEnd, DragInCancel]),
            Target::Out => out.extend([DragOutEnd, DragOutCancel]),
        }
        out.extend(
            self.views
                .iter()
                .map(|&(zoom, scroll)| ZoomChange(zoom, scroll)),
        );
        if s.stopped_clock.is_some() && s.drag_target == Target::None {
            out.push(SampleStoppedClock);
        }
        if s.pending_preview_frame.is_some() && s.late_frame_in_flight.is_none() {
            out.push(DecodeStart);
        }
        if s.late_frame_in_flight.is_some() {
            out.push(LateFrameArrives);
        }
    }

    fn next_state(&self, before: &State, action: Action) -> Option<State> {
        use Action::*;
        let mut s = *before;
        let idle = before.drag_target == Target::None;
        match action {
            DragPlayheadStart(t) if idle => {
                s.stopped_clock = None;
                s.drag_target = Target::Playhead;
                s.playhead_ns = t;
            }
            DragPlayheadMove(t) if s.drag_target == Target::Playhead => s.playhead_ns = t,
            DragPlayheadEnd if s.drag_target == Target::Playhead => s.drag_target = Target::None,
            DragInStart(t) | DragInMove(t) => {
                self.drag(&mut s, Target::In, t, action)?;
            }
            DragOutStart(t) | DragOutMove(t) => {
                self.drag(&mut s, Target::Out, t, action)?;
            }
            DragInEnd | DragInCancel | DragOutEnd | DragOutCancel => {
                let edge = if matches!(action, DragInEnd | DragInCancel) {
                    Target::In
                } else {
                    Target::Out
                };
                if s.drag_target != edge {
                    return None;
                }
                let cancel = matches!(action, DragInCancel | DragOutCancel);
                if cancel {
                    self.set_edge(&mut s, edge, before.original);
                } else if self.mutation == Mutation::Snap {
                    let time = before.preview_time_ns?;
                    let snapped = if edge == Target::In {
                        self.starts[self.frame(time)]
                    } else {
                        self.boundary(time - 1, 1)
                    };
                    self.set_edge(&mut s, edge, snapped);
                }
                s.drag_target = Target::None;
                s.original = 0;
                if !cancel || self.mutation != Mutation::Leaked {
                    s.preview_time_ns = None;
                }
                Self::invalidate(&mut s);
            }
            NudgeIn(d) | NudgeOut(d) | FrameIn(d) | FrameOut(d) if idle => {
                let (edge, from) = if matches!(action, NudgeIn(_) | FrameIn(_)) {
                    (Target::In, s.in_ns)
                } else {
                    (Target::Out, s.out_ns)
                };
                let time = if matches!(action, FrameIn(_) | FrameOut(_)) {
                    self.boundary(from, d)
                } else {
                    from + self.step * i64::from(d)
                };
                self.set_edge(&mut s, edge, time);
            }
            MarkInFromPlayhead if idle => {
                self.set_edge(&mut s, Target::In, before.playhead_ns);
            }
            MarkOutFromPlayhead if idle => {
                self.set_edge(&mut s, Target::Out, before.playhead_ns);
            }
            TogglePlay if idle => {
                s.stopped_clock = None;
                s.playing = !s.playing;
            }
            PlaybackStopsAt(t) if s.playing => {
                s.playing = false;
                if matches!(s.drag_target, Target::In | Target::Out) {
                    s.stopped_clock = Some(t);
                } else if idle {
                    s.playhead_ns = t;
                }
            }
            SampleStoppedClock if idle => s.playhead_ns = s.stopped_clock.take()?,
            PlayTick(t) if s.playing => {
                if idle {
                    s.playhead_ns = t;
                }
            }
            DecodeStart if s.late_frame_in_flight.is_none() => {
                s.pending_preview_frame.take()?;
                s.late_frame_in_flight = Some(true);
            }
            LateFrameArrives => {
                if s.late_frame_in_flight.take()? {
                    s.preview_displayed = true;
                }
            }
            ZoomChange(zoom, scroll) => {
                s.zoom = zoom;
                s.scroll = scroll;
            }
            _ => return None,
        }
        self.check_transition(before, action, &mut s);
        Some(s)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always("S7-1", |_, s| s.violation != Violation::Coupled),
            Property::<Self>::always("S7-2", |_, s| s.violation != Violation::Snapped),
            Property::<Self>::always("S7-3", |_, s| s.violation != Violation::Contaminated),
            Property::<Self>::always("S7-4", |m, s| {
                s.in_ns >= 0 && s.out_ns <= m.end && s.out_ns - s.in_ns >= m.minimum
            }),
            Property::<Self>::always("S7-5", |_, s| {
                matches!(s.drag_target, Target::In | Target::Out)
                    || (s.preview_time_ns.is_none()
                        && !s.preview_displayed
                        && s.pending_preview_frame.is_none())
            }),
            Property::<Self>::sometimes("playing with stale decode and new preview", |_, s| {
                s.playing
                    && s.pending_preview_frame.is_some()
                    && s.late_frame_in_flight == Some(false)
            }),
        ]
    }
}

#[test]
fn exhaustive_s7() {
    let start = Instant::now();
    let checker = S7::bounded(Mutation::None).checker().spawn_bfs().join();
    checker.assert_properties();
    println!(
        "S7: {} states in {:?}",
        checker.unique_state_count(),
        start.elapsed()
    );
}

#[test]
fn negative_controls() {
    for (mutation, name) in [
        (Mutation::Coupled, "S7-1"),
        (Mutation::Snap, "S7-2"),
        (Mutation::Leaked, "S7-5"),
    ] {
        let checker = S7::bounded(mutation)
            .checker()
            .threads(1)
            .finish_when(HasDiscoveries::AnyFailures)
            .spawn_bfs()
            .join();
        let path = checker.assert_any_discovery(name);
        assert!(!(checker
            .model()
            .properties()
            .into_iter()
            .find(|p| p.name == name)
            .unwrap()
            .condition)(checker.model(), path.last_state()));
        println!("{name} {mutation:?}: {:?}", path.into_actions());
    }
}

#[test]
fn shared_stateright_paths() {
    use serde_json::{Value, json};
    let m = S7::recording();
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/specs/studio-trim-s7-traces.json");
    let stored: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let cases = stored["traces"].as_array().unwrap();
    assert!(cases.len() >= 16);
    let mut traces = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let actions: Vec<Action> = case["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| serde_json::from_value(step["action"].clone()).unwrap())
            .collect();
        let path = stateright::Path::from_actions(&m, m.init_states()[0], &actions)
            .unwrap_or_else(|| panic!("unreachable schedule {name}"));
        let steps: Vec<_> = actions.iter().zip(path.into_states().into_iter().skip(1)).map(|(action, s)| {
            let ms = |ns: i64| {
                assert_eq!(ns % 1_000_000, 0, "fixture pixel inputs are whole milliseconds");
                f64::from(i32::try_from(ns / 1_000_000).unwrap())
            };
            let pixel = |t| 14.0 + ms(t - s.scroll) / ms(m.end / i64::from(s.zoom)) * 1024.0;
            json!({"action":action,"playheadNs":s.playhead_ns,"inNs":s.in_ns,"outNs":s.out_ns,
                "previewNs":s.preview_time_ns,"inX":pixel(s.in_ns),"outX":pixel(s.out_ns),"playheadX":pixel(s.playhead_ns),
                "previewDisplayed":s.preview_displayed})
        }).collect();
        traces.push(json!({"name":name,"steps":steps}));
    }
    let fixture =
        json!({"endNs":m.end,"frameStartsNs":m.starts,"leftPx":14,"widthPx":1024,"traces":traces});
    if std::env::var_os("ARB_REGENERATE_S7").is_some() {
        std::fs::write(
            &file,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();
    }
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(fixture, stored, "regenerate with ARB_REGENERATE_S7=1");
}
