//! Stable pages and display paths of the bundle catalog (#586-E), shared by `bundles list` and
//! `arb_list_bundles`.
//!
//! The daemon's `bundle.list` stays the complete list the IDE reads. A page orders it newest
//! first by `(created_at_utc, bundle_id)` and resumes strictly after the cursor's key, so walking
//! the cursors returns every row exactly once even when bundles are saved in between (new rows
//! sort before the cursor and appear on a fresh first page).
use serde_json::Value;

/// Rows per `arb_list_bundles` page when `limit` is omitted.
pub(crate) const DEFAULT_MCP_LIMIT: usize = 20;
/// Largest accepted `limit`.
pub(crate) const MAX_LIMIT: usize = 200;

/// One page of catalog rows.
#[derive(Debug)]
pub(crate) struct Page {
    pub(crate) rows: Vec<Value>,
    pub(crate) total_count: usize,
    pub(crate) has_more: bool,
    pub(crate) next_cursor: Option<String>,
}

/// A cursor that this module did not issue.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvalidCursor;

/// Sort key: creation instant in Unix ms (unknown first-written rows sort last) and the id.
type Key = (i64, String);

fn key(row: &Value) -> Key {
    let created = row["created_at_utc"]
        .as_str()
        .and_then(arb_core::time::parse_utc_iso8601_ms)
        .map_or(i64::MIN, |(ms, _)| ms);
    (
        created,
        row["bundle_id"].as_str().unwrap_or_default().to_owned(),
    )
}

fn encode((ms, id): &Key) -> String {
    format!("{ms}~{id}")
}

fn decode(cursor: &str) -> Result<Key, InvalidCursor> {
    let (ms, id) = cursor.split_once('~').ok_or(InvalidCursor)?;
    let ms = ms.parse::<i64>().map_err(|_| InvalidCursor)?;
    if id.is_empty() {
        return Err(InvalidCursor);
    }
    Ok((ms, id.to_owned()))
}

/// Orders `rows` newest first and returns at most `limit` rows after `cursor`.
///
/// # Errors
///
/// [`InvalidCursor`] when `cursor` is not a `next_cursor` this module produced.
pub(crate) fn paginate(
    rows: Vec<Value>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<Page, InvalidCursor> {
    let after = cursor.map(decode).transpose()?;
    let total_count = rows.len();
    let mut keyed: Vec<(Key, Value)> = rows.into_iter().map(|row| (key(&row), row)).collect();
    keyed.sort_by(|a, b| b.0.cmp(&a.0));
    let remaining: Vec<_> = keyed
        .into_iter()
        .filter(|(k, _)| after.as_ref().is_none_or(|after| k < after))
        .collect();
    let limit = limit.max(1);
    let has_more = remaining.len() > limit;
    let mut rows = Vec::with_capacity(limit.min(remaining.len()));
    let mut last = None;
    for (k, row) in remaining.into_iter().take(limit) {
        last = Some(k);
        rows.push(row);
    }
    Ok(Page {
        rows,
        total_count,
        has_more,
        next_cursor: last.filter(|_| has_more).map(|k| encode(&k)),
    })
}

/// Replaces a catalog row's `path` with its display form ([`arb_os::display_path_str`]). The
/// catalog keeps the canonical path for comparisons; only the published text changes.
pub(crate) fn display_row_path(row: &mut Value) {
    if let Some(Value::String(path)) = row.get_mut("path") {
        let display = arb_os::display_path_str(path).into_owned();
        *path = display;
    }
}

#[cfg(test)]
mod tests {
    use super::{InvalidCursor, display_row_path, paginate};
    use proptest::prelude::*;
    use serde_json::{Value, json};

    fn row(id: &str, created: &str) -> Value {
        json!({"bundle_id": id, "created_at_utc": created, "path": format!("/b/{id}")})
    }

    fn ids(rows: &[Value]) -> Vec<&str> {
        rows.iter()
            .map(|r| r["bundle_id"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn newest_first_with_id_ties_and_unknown_times_last() {
        let rows = vec![
            row("a", "2026-11-01T00:00:00.000Z"),
            row("c", "2026-11-02T00:00:00Z"),
            row("b", "2026-11-02T00:00:00.000Z"),
            row("legacy", "not a time"),
            row("d", "2026-11-01T00:00:00.500Z"),
        ];
        let page = paginate(rows, 10, None).unwrap();
        assert_eq!(ids(&page.rows), ["c", "b", "d", "a", "legacy"]);
        assert_eq!(page.total_count, 5);
        assert!(!page.has_more);
        assert_eq!(page.next_cursor, None);
    }

    #[test]
    fn cursors_resume_after_the_last_row_even_when_rows_are_added() {
        let mut rows: Vec<_> = (0..5)
            .map(|i| {
                row(
                    &format!("b{i}"),
                    &format!("2026-11-0{}T00:00:00.000Z", i + 1),
                )
            })
            .collect();
        let first = paginate(rows.clone(), 2, None).unwrap();
        assert_eq!(ids(&first.rows), ["b4", "b3"]);
        assert!(first.has_more);
        // A newer save between pages does not shift the next page.
        rows.push(row("new", "2026-12-01T00:00:00.000Z"));
        let second = paginate(rows.clone(), 2, first.next_cursor.as_deref()).unwrap();
        assert_eq!(ids(&second.rows), ["b2", "b1"]);
        assert_eq!(second.total_count, 6);
        let third = paginate(rows, 2, second.next_cursor.as_deref()).unwrap();
        assert_eq!(ids(&third.rows), ["b0"]);
        assert!(!third.has_more);
        assert_eq!(third.next_cursor, None);
    }

    #[test]
    fn foreign_cursors_are_rejected() {
        for cursor in ["", "abc", "12", "x~id", "12~"] {
            assert_eq!(
                paginate(vec![], 1, Some(cursor)).unwrap_err(),
                InvalidCursor,
                "{cursor}"
            );
        }
    }

    #[test]
    fn display_paths_drop_only_the_verbatim_prefix() {
        let mut windows = json!({"path": r"\\?\C:\Users\me\arb\bundles\b1"});
        display_row_path(&mut windows);
        assert_eq!(windows["path"], r"C:\Users\me\arb\bundles\b1");
        let mut unix = json!({"path": "/home/me/b1"});
        display_row_path(&mut unix);
        assert_eq!(unix["path"], "/home/me/b1");
    }

    proptest! {
        /// Walking every cursor returns each row exactly once, newest first.
        #[test]
        fn walking_cursors_returns_every_row_once(
            times in proptest::collection::vec(0_u32..50, 0..40),
            limit in 1_usize..8,
        ) {
            let rows: Vec<_> = times
                .iter()
                .enumerate()
                .map(|(i, t)| row(&format!("id{i:02}"), &format!("2026-11-01T00:00:{:02}.000Z", t % 60)))
                .collect();
            let mut seen = Vec::new();
            let mut cursor = None;
            loop {
                let page = paginate(rows.clone(), limit, cursor.as_deref()).unwrap();
                prop_assert!(page.rows.len() <= limit);
                prop_assert_eq!(page.total_count, rows.len());
                seen.extend(page.rows.iter().map(|r| r["bundle_id"].as_str().unwrap().to_owned()));
                if !page.has_more {
                    prop_assert!(page.next_cursor.is_none());
                    break;
                }
                cursor = page.next_cursor;
            }
            let mut expected: Vec<_> = rows.iter().map(|r| (r["created_at_utc"].as_str().unwrap().to_owned(), r["bundle_id"].as_str().unwrap().to_owned())).collect();
            expected.sort_by(|a, b| b.cmp(a));
            let expected: Vec<_> = expected.into_iter().map(|(_, id)| id).collect();
            prop_assert_eq!(seen, expected);
        }
    }
}
