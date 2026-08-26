//! Invariant: every function here is PURE. A `gh` JSON payload becomes a [`Collected`] with its
//! ref, its refs and its class decided by data — never by a clock, a network read or a global — so
//! the class rules (§5: pushes, CI and state changes never wake a dormant agent) are testable
//! against fixtures alone.

use std::collections::BTreeSet;

use bough_plugin_agents::MailClass;
use bough_plugin_collect_core::{refs, Collected, WakeClass};
use chrono::{DateTime, Utc};

/// The sources this collector sweeps; each has its own watermark, per repo.
pub const SOURCES: [&str; 4] = ["prs", "review_requests", "mentions", "checks"];

/// The `--json` field list of the `prs` sweep. A protocol detail, not a knob (§0.2).
pub const PR_FIELDS: [&str; 7] = [
    "number",
    "title",
    "url",
    "updatedAt",
    "author",
    "state",
    "isDraft",
];

/// The `--json` field list of the `checks` sweep.
pub const CHECK_FIELDS: [&str; 4] = ["number", "title", "url", "statusCheckRollup"];

/// PURE: an RFC3339 timestamp becomes an ordering key in millis. `None` when it is unreadable.
pub fn ts(value: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = value?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn str_of<'a>(row: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(|v| v.as_str())
}

fn number_of(row: &serde_json::Value) -> Option<u64> {
    row.get("number").and_then(|v| v.as_u64())
}

/// PURE: one `gh pr list` row becomes a [`Collected`]. A row missing its number or its timestamp
/// is `None` — the sweep counts it as unusable rather than inventing an id.
pub fn pr_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    let number = number_of(row)?;
    let at = ts(str_of(row, "updatedAt"))?;
    let title = str_of(row, "title").unwrap_or("").to_string();
    let author = row
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("");
    let state = str_of(row, "state").unwrap_or("OPEN");
    let draft = row
        .get("isDraft")
        .and_then(|d| d.as_bool())
        .unwrap_or(false);
    let url = str_of(row, "url").map(|s| s.to_string());
    let r = refs::pr(repo, number);
    Some(Collected {
        subject: format!("{repo}#{number} {title}"),
        summary: format!("{state}{} by {author}", if draft { " (draft)" } else { "" }),
        text: format!(
            "{repo}#{number} {title}\n{state} by {author}\n{}",
            url.clone().unwrap_or_default()
        ),
        refs: BTreeSet::from([r.clone()]),
        r#ref: r,
        url,
        // §5: a PR state change is never a wake on its own.
        class: MailClass::Ordinary,
        at,
        order: at.timestamp_millis(),
    })
}

/// PURE: one `search/issues` item becomes a [`Collected`] for a source that names a [`WakeClass`].
/// Shared by `review_requests` and `mentions`, which differ only in the query and the class.
fn search_item_of(repo: &str, row: &serde_json::Value, what: &str) -> Option<Collected> {
    let number = number_of(row)?;
    let at = ts(str_of(row, "updated_at"))?;
    let title = str_of(row, "title").unwrap_or("").to_string();
    let user = row
        .get("user")
        .and_then(|u| u.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("");
    let url = str_of(row, "html_url").map(|s| s.to_string());
    let body = str_of(row, "body").unwrap_or("");
    let r = refs::pr(repo, number);
    Some(Collected {
        subject: format!("{what}: {repo}#{number} {title}"),
        summary: format!("{what} from {user}"),
        text: format!(
            "{what}: {repo}#{number} {title}\nfrom {user}\n{}\n\n{body}",
            url.clone().unwrap_or_default()
        ),
        refs: BTreeSet::from([r.clone()]),
        r#ref: r,
        url,
        // Overwritten by `class_of` at the sweep, from the row's configured `wake_classes`.
        class: MailClass::Ordinary,
        at,
        order: at.timestamp_millis(),
    })
}

/// PURE: one review request becomes a [`Collected`].
pub fn review_request_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    search_item_of(repo, row, "review requested")
}

/// PURE: one `@`-mention becomes a [`Collected`].
pub fn mention_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    search_item_of(repo, row, "mention")
}

/// PURE: one `gh pr list --json …,statusCheckRollup` row becomes a [`Collected`] for its FIRST
/// failing check run, or `None` when nothing is failing. NEVER wake-class (§5).
pub fn check_of(repo: &str, row: &serde_json::Value) -> Option<Collected> {
    let number = number_of(row)?;
    let title = str_of(row, "title").unwrap_or("").to_string();
    let url = str_of(row, "url").map(|s| s.to_string());
    let rollup = row.get("statusCheckRollup")?.as_array()?;
    let failed = rollup.iter().find(|c| {
        let concl = str_of(c, "conclusion").unwrap_or("");
        matches!(
            concl,
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
        )
    })?;
    let name = str_of(failed, "name").unwrap_or("check").to_string();
    let concl = str_of(failed, "conclusion")
        .unwrap_or("FAILURE")
        .to_string();
    let at = ts(str_of(failed, "completedAt")).or_else(|| ts(str_of(row, "updatedAt")))?;
    let r = refs::check(repo, number, &name);
    Some(Collected {
        subject: format!("{repo}#{number} check {name} {concl}"),
        summary: format!("{name} {concl}"),
        text: format!(
            "{repo}#{number} {title}\ncheck `{name}` {concl}\n{}",
            url.clone().unwrap_or_default()
        ),
        refs: BTreeSet::from([r.clone(), refs::pr(repo, number)]),
        r#ref: r,
        url,
        // §5 is explicit: CI is never a wake.
        class: MailClass::Ordinary,
        at,
        order: at.timestamp_millis(),
    })
}

/// PURE: the mail class of a collected item, given the row's configured wake classes. Everything
/// not named is [`MailClass::Ordinary`] (§5).
pub fn class_of(kind: WakeClass, wake_classes: &[WakeClass]) -> MailClass {
    if wake_classes.contains(&kind) {
        MailClass::Wake
    } else {
        MailClass::Ordinary
    }
}

/// PURE: the `search/issues` query one source sends for one repo.
pub fn search_query(source: &str, repo: &str) -> String {
    match source {
        "review_requests" => format!("is:open is:pr review-requested:@me repo:{repo}"),
        _ => format!("is:open mentions:@me repo:{repo}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr_row() -> serde_json::Value {
        serde_json::json!({
            "number": 12,
            "title": "a PR",
            "url": "https://example.invalid/12",
            "updatedAt": "2026-08-01T00:00:00Z",
            "author": { "login": "andrey" },
            "state": "OPEN",
            "isDraft": false
        })
    }

    #[test]
    fn a_pr_row_becomes_an_ordinary_cited_item() {
        let c = pr_of("o/r", &pr_row()).expect("a pr");
        assert_eq!(c.r#ref.as_str(), "gh:o/r#12");
        assert_eq!(c.class, MailClass::Ordinary);
        assert_eq!(c.order, c.at.timestamp_millis());
        assert!(c.refs.contains(&refs::pr("o/r", 12)));
    }

    #[test]
    fn a_pr_row_without_a_timestamp_is_unusable_rather_than_invented() {
        let mut row = pr_row();
        row["updatedAt"] = serde_json::json!("not a time");
        assert!(pr_of("o/r", &row).is_none());
    }

    #[test]
    fn a_review_request_carries_its_pr_ref() {
        let row = serde_json::json!({
            "number": 4, "title": "fix", "updated_at": "2026-08-01T00:00:00Z",
            "html_url": "https://example.invalid/4", "user": { "login": "teammate" }, "body": "please look"
        });
        let c = review_request_of("o/r", &row).expect("an item");
        assert_eq!(c.r#ref.as_str(), "gh:o/r#4");
        assert!(c.subject.contains("review requested"));
    }

    #[test]
    fn a_green_rollup_collects_nothing_and_a_failure_is_never_wake_class() {
        let green = serde_json::json!({
            "number": 4, "title": "fix", "url": "u",
            "statusCheckRollup": [{ "name": "test", "conclusion": "SUCCESS" }]
        });
        assert!(check_of("o/r", &green).is_none());
        let red = serde_json::json!({
            "number": 4, "title": "fix", "url": "u", "updatedAt": "2026-08-01T00:00:00Z",
            "statusCheckRollup": [{ "name": "test", "conclusion": "FAILURE" }]
        });
        let c = check_of("o/r", &red).expect("a failing check");
        assert_eq!(c.r#ref.as_str(), "gh:o/r#4:check:test");
        assert_eq!(c.class, MailClass::Ordinary);
    }

    #[test]
    fn only_a_configured_wake_class_wakes() {
        let cfg = [WakeClass::ReviewRequest];
        assert_eq!(class_of(WakeClass::ReviewRequest, &cfg), MailClass::Wake);
        assert_eq!(class_of(WakeClass::Mention, &cfg), MailClass::Ordinary);
        assert_eq!(class_of(WakeClass::Assigned, &[]), MailClass::Ordinary);
    }

    #[test]
    fn the_two_search_sources_send_two_different_queries() {
        assert!(search_query("review_requests", "o/r").contains("review-requested:@me"));
        assert!(search_query("mentions", "o/r").contains("mentions:@me"));
    }
}
