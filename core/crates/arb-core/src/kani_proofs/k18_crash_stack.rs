//! K18 (#589): the crash `stack_top` keeps the first app frame and never exceeds its cap
//! (`crate::analytics::crash_detail::plan_stack_top`).
//!
//! The oracle (the first `true` position; the leading frames; the cap) is stated here
//! independently of the production selection. `enrich_crash_marker` emits exactly the frames
//! `0..head` followed by `extra`, and `first_app_frame` is the frame at `app`.

use crate::analytics::{MAX_STACK_FRAMES, MAX_STACK_TOP_LINES, plan_stack_top};

#[kani::proof]
#[kani::unwind(15)]
pub fn k18_stack_top_keeps_the_first_app_frame_within_the_cap() {
    let flags: [bool; 13] = kani::any();
    let len: usize = kani::any();
    kani::assume(len <= flags.len());
    let frames = &flags[..len];
    let plan = plan_stack_top(frames);

    assert!(plan.len() <= MAX_STACK_TOP_LINES, "stack_top is bounded");
    assert_eq!(
        plan.len() == 0,
        len == 0,
        "stack_top is empty only without frames"
    );
    assert_eq!(
        plan.head,
        len.min(MAX_STACK_FRAMES),
        "the leading frames are kept"
    );
    let mut first = None;
    let mut i = 0;
    while i < len {
        if first.is_none() && frames[i] {
            first = Some(i);
        }
        i += 1;
    }
    assert_eq!(
        plan.app, first,
        "first_app_frame is the first app frame, or none"
    );
    if let Some(a) = plan.app {
        assert!(
            a < plan.head || plan.extra == Some(a),
            "first_app_frame is in stack_top"
        );
    }
    if let Some(e) = plan.extra {
        assert!(
            e >= plan.head && e < len,
            "the appended frame is a deeper frame"
        );
    }
    kani::cover!(plan.extra.is_some(), "deep app frame appended");
    kani::cover!(
        plan.app.is_some_and(|a| a < MAX_STACK_FRAMES),
        "app frame among the leading frames"
    );
    kani::cover!(
        plan.app.is_none() && len > MAX_STACK_FRAMES,
        "no app frame in a long stack"
    );
}
