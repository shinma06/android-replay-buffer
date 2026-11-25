//! Layer-3 PDD (G1): real SQLite/WAL catalog, two connections, random op sequences,
//! checked against an independent in-memory reference model.

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

#[path = "support/catalog_fixture.rs"]
mod catalog_fixture;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use arb_core::state::SaveJobState;
use arb_storage::catalog::{
    Catalog, CatalogFormatVersion, LeaseHolderKind, NewBundleEntry, parse_utc_timestamp_ns,
};
use proptest::prelude::*;

const GRACE_SECS: u64 = 24 * 3600;
const BUNDLES: usize = 3;

fn ts(hours: u32) -> String {
    format!("2026-01-{:02}T{:02}:00:00Z", 1 + hours / 24, hours % 24)
}

#[derive(Debug, Clone)]
enum Op {
    Register(usize),
    Trash(usize, u32),
    Restore(usize),
    AcquireLease(usize, bool),
    ReleaseLease(usize, bool),
    Gc(u32, bool),
    CreateJob(usize),
    Fail(usize),
    Retry(usize),
    Retarget(usize),
    Discard(usize),
    Complete(usize),
    Recover,
}

fn op() -> impl Strategy<Value = Op> {
    let b = 0..BUNDLES;
    prop_oneof![
        b.clone().prop_map(Op::Register),
        (b.clone(), 0u32..96).prop_map(|(i, h)| Op::Trash(i, h)),
        b.clone().prop_map(Op::Restore),
        (b.clone(), any::<bool>()).prop_map(|(i, c)| Op::AcquireLease(i, c)),
        (0usize..4, any::<bool>()).prop_map(|(i, c)| Op::ReleaseLease(i, c)),
        (0u32..120, any::<bool>()).prop_map(|(h, c)| Op::Gc(h, c)),
        b.clone().prop_map(Op::CreateJob),
        b.clone().prop_map(Op::Fail),
        b.clone().prop_map(Op::Retry),
        b.clone().prop_map(Op::Retarget),
        b.clone().prop_map(Op::Discard),
        b.prop_map(Op::Complete),
        Just(Op::Recover),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefBundle {
    Active,
    Trashed(u32),
    Purged,
}

#[derive(Default)]
struct Reference {
    bundles: BTreeMap<usize, RefBundle>,
    /// (lease id, bundle, heartbeat hour); TTL is 600 s, so any later hour expires it.
    leases: Vec<(String, usize, u32)>,
    /// job index -> (state, destination is bundle path)
    jobs: BTreeMap<usize, (SaveJobState, bool)>,
}

impl Reference {
    fn reap(&mut self, now: u32) {
        self.leases.retain(|(_, _, at)| now == *at);
    }
    fn active_job_on(&self, b: usize) -> bool {
        self.jobs
            .get(&b)
            .is_some_and(|(s, on_bundle)| *on_bundle && !s.is_terminal())
    }
}

fn job_transition(from: SaveJobState, op: &Op) -> Option<SaveJobState> {
    use SaveJobState as S;
    match (op, from) {
        (Op::Fail(_), S::Pinned | S::Draining | S::Materializing) => Some(S::FailedRetryable),
        (Op::Retry(_) | Op::Retarget(_), S::FailedRetryable) => Some(S::Materializing),
        // An in-flight materialize must fail first; it cannot be discarded underneath.
        (Op::Discard(_), S::Pinned | S::Draining | S::FailedRetryable) => Some(S::Discarded),
        _ => None,
    }
}

fn run_ops(ops: &[Op]) -> Result<(), TestCaseError> {
    let tmp = tempfile::tempdir().map_err(|e| TestCaseError::fail(e.to_string()))?;
    let managed = tmp.path().join("bundles");
    std::fs::create_dir_all(&managed).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let db = tmp.path().join("catalog.sqlite");
    let c1 = Catalog::open(&db, &managed).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let c2 = Catalog::open(&db, &managed).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let path = |b: usize| -> PathBuf { managed.join(format!("b{b}.arb")) };
    let mut r = Reference::default();
    let mut now = 0u32;

    for (step, op) in ops.iter().enumerate() {
        let cat = if step % 2 == 0 { &c1 } else { &c2 };
        let t = ts(now);
        match *op {
            Op::Register(b) => {
                std::fs::create_dir_all(path(b)).map_err(|e| TestCaseError::fail(e.to_string()))?;
                let res = cat.register_bundle(&NewBundleEntry {
                    bundle_id: format!("b{b}"),
                    path: path(b),
                    format_version: CatalogFormatVersion::V2,
                    parent_bundle_id: None,
                    created_at_utc: t.clone(),
                });
                let expect_ok = matches!(r.bundles.get(&b), None | Some(RefBundle::Purged));
                prop_assert_eq!(res.is_ok(), expect_ok, "register {:?}", op);
                if expect_ok {
                    catalog_fixture::write_manifest(&path(b), &format!("b{b}"), &t);
                    r.bundles.insert(b, RefBundle::Active);
                }
            }
            Op::Trash(b, h) => {
                now = now.max(h);
                let res = cat.trash_bundle(&format!("b{b}"), &ts(now));
                let expect_ok = r.bundles.get(&b) == Some(&RefBundle::Active);
                prop_assert_eq!(res.is_ok(), expect_ok, "trash {:?}", op);
                if expect_ok {
                    r.bundles.insert(b, RefBundle::Trashed(now));
                }
            }
            Op::Restore(b) => {
                let res = cat.restore_bundle(&format!("b{b}"));
                let expect_ok = matches!(r.bundles.get(&b), Some(RefBundle::Trashed(_)));
                prop_assert_eq!(res.is_ok(), expect_ok, "restore {:?}", op);
                if expect_ok {
                    r.bundles.insert(b, RefBundle::Active);
                }
            }
            Op::AcquireLease(b, second) => {
                let cat = if second { &c2 } else { &c1 };
                let res = cat.acquire_lease(
                    &format!("b{b}"),
                    LeaseHolderKind::IdeViewer,
                    std::process::id(),
                    &t,
                );
                let live = matches!(
                    r.bundles.get(&b),
                    Some(RefBundle::Active | RefBundle::Trashed(_))
                );
                if !live {
                    prop_assert!(res.is_err(), "lease on missing/purged bundle must fail");
                } else if let Ok(id) = res {
                    r.leases.push((id, b, now));
                }
            }
            Op::ReleaseLease(i, second) => {
                if i < r.leases.len() {
                    let (id, _, _) = r.leases.remove(i);
                    let cat = if second { &c2 } else { &c1 };
                    prop_assert!(
                        cat.release_lease(&id)
                            .map_err(|e| TestCaseError::fail(e.to_string()))?
                    );
                }
            }
            Op::Gc(h, second) => {
                now = now.max(h);
                let cat = if second { &c2 } else { &c1 };
                let report = cat
                    .gc_with_grace(&ts(now), GRACE_SECS)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                r.reap(now);
                let leased: BTreeSet<usize> = r.leases.iter().map(|(_, b, _)| *b).collect();
                let expected: BTreeSet<String> = r
                    .bundles
                    .iter()
                    .filter(|(b, s)| match s {
                        RefBundle::Trashed(at) => {
                            u64::from(now - at) * 3600 >= GRACE_SECS
                                && !leased.contains(b)
                                && !r.active_job_on(**b)
                        }
                        _ => false,
                    })
                    .map(|(b, _)| format!("b{b}"))
                    .collect();
                let got: BTreeSet<String> = report.purged_bundle_ids.iter().cloned().collect();
                prop_assert_eq!(&got, &expected, "gc at {}h", now);
                for id in &expected {
                    let b: usize = id[1..].parse().map_err(|_| TestCaseError::fail("id"))?;
                    r.bundles.insert(b, RefBundle::Purged);
                    prop_assert!(!path(b).exists(), "purged bundle directory removed");
                }
                for (b, s) in &r.bundles {
                    if *s != RefBundle::Purged {
                        prop_assert!(path(*b).exists(), "non-purged bundle untouched");
                    }
                }
            }
            Op::CreateJob(j) => {
                let res = cat.create_pinned_job(&format!("j{j}"), path(j), &[format!("c{j}")], &t);
                let expect_ok = !r.jobs.contains_key(&j);
                prop_assert_eq!(res.is_ok(), expect_ok, "create job {:?}", op);
                if expect_ok {
                    r.jobs.insert(j, (SaveJobState::Pinned, true));
                }
            }
            Op::Fail(j) | Op::Retry(j) | Op::Retarget(j) | Op::Discard(j) | Op::Complete(j) => {
                let id = format!("j{j}");
                let res = match op {
                    Op::Fail(_) => cat.mark_failed_retryable(&id, "disk full", &t),
                    Op::Retry(_) => cat.retry_job(&id, &t),
                    Op::Retarget(_) => {
                        cat.retarget_job(&id, tmp.path().join(format!("ext{j}")), &t)
                    }
                    Op::Discard(_) => cat.discard_job(&id, &t),
                    _ => cat.complete_job(&id, &t),
                };
                let Some(&(state, on_bundle)) = r.jobs.get(&j) else {
                    prop_assert!(res.is_err(), "unknown job must fail");
                    continue;
                };
                if matches!(op, Op::Complete(_)) {
                    // Completion requires the materializing phase; only check soundness.
                    if let Ok(job) = res {
                        prop_assert_eq!(job.job.state, SaveJobState::Completed);
                        prop_assert!(state.can_transition_to(SaveJobState::Completed));
                        prop_assert!(job.job.pinned_chunk_ids.is_empty());
                        r.jobs.insert(j, (SaveJobState::Completed, on_bundle));
                    }
                    continue;
                }
                let expected = job_transition(state, op);
                prop_assert_eq!(res.is_ok(), expected.is_some(), "{:?} from {:?}", op, state);
                if let (Ok(job), Some(next)) = (res, expected) {
                    prop_assert_eq!(job.job.state, next);
                    if next.is_terminal() {
                        prop_assert!(job.job.pinned_chunk_ids.is_empty());
                    } else {
                        prop_assert_eq!(job.job.pinned_chunk_ids, vec![format!("c{j}")]);
                    }
                    let on = on_bundle && !matches!(op, Op::Retarget(_));
                    r.jobs.insert(j, (next, on));
                }
            }
            Op::Recover => {
                cat.recover_on_startup(&t)
                    .map_err(|e| TestCaseError::fail(e.to_string()))?;
                r.reap(now);
                for (j, (state, _)) in &r.jobs {
                    let got = cat
                        .save_job(&format!("j{j}"))
                        .map_err(|e| TestCaseError::fail(e.to_string()))?;
                    let got = got.ok_or_else(|| TestCaseError::fail("job vanished"))?;
                    prop_assert_eq!(got.job.state.is_terminal(), state.is_terminal());
                    if !state.is_terminal() {
                        prop_assert_eq!(got.job.pinned_chunk_ids, vec![format!("c{j}")]);
                    }
                }
                let updated: Vec<(usize, SaveJobState)> = r
                    .jobs
                    .keys()
                    .filter_map(|j| {
                        cat.save_job(&format!("j{j}"))
                            .ok()
                            .flatten()
                            .map(|x| (*j, x.job.state))
                    })
                    .collect();
                for (j, s) in updated {
                    if let Some(e) = r.jobs.get_mut(&j) {
                        e.0 = s;
                    }
                }
            }
        }
        // Both connections observe the same committed bundle states (WAL visibility).
        for b in r.bundles.keys() {
            let a = c1
                .bundle(&format!("b{b}"))
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            let z = c2
                .bundle(&format!("b{b}"))
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(a.map(|x| x.state), z.map(|x| x.state));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(32),
        ..ProptestConfig::default()
    })]

    #[test]
    fn g1_catalog_matches_reference_model(ops in prop::collection::vec(op(), 1..=32)) {
        run_ops(&ops)?;
    }
}

#[test]
fn g1_strict_calendar_and_offset_rejection() {
    for bad in [
        "2026-02-31T00:00:00Z",
        "2026-02-29T00:00:00Z",
        "2026-13-01T00:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T00:00:00+99:99",
        "2026-01-01T00:00:00+24:00",
    ] {
        assert!(
            parse_utc_timestamp_ns(bad).is_none(),
            "{bad} must be rejected"
        );
    }
    let a = parse_utc_timestamp_ns("2026-01-01T00:00:00Z").expect("valid");
    let b = parse_utc_timestamp_ns("2026-01-02T00:00:00+00:00").expect("valid");
    assert_eq!(
        b - a,
        i128::from(GRACE_SECS) * 1_000_000_000,
        "24h grace arithmetic"
    );
    assert!(
        parse_utc_timestamp_ns("2028-02-29T00:00:00Z").is_some(),
        "leap day accepted"
    );
}
