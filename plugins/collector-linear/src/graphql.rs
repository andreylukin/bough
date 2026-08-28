//! Invariant: the two queries this row sends are CONSTANTS in this file — a protocol detail, not a
//! deployment knob (§0.2) — and parsing is PURE, so the whole collector is testable against a
//! local stub and recorded payloads.

use std::collections::BTreeSet;

use bough_plugin_agents::MailClass;
use bough_plugin_collect_core::{refs, Collected};
use chrono::{DateTime, Utc};

/// Issues assigned to the authenticated user, inside the row's configured scope.
///
/// The `$filter` is what makes the doc comment true. Without it this query returned EVERY issue
/// in the workspace and the sweep stamped `WakeClass::Assigned` on all of them, so a row scoped
/// to one team woke its `deliver_to` agent for every ticket in the org.
pub const ISSUES_QUERY: &str =
    "query BoughIssues($after: String, $first: Int!, $filter: IssueFilter) { \
issues(first: $first, after: $after, filter: $filter) { \
pageInfo { hasNextPage endCursor } \
nodes { id identifier title url updatedAt description assignee { name } state { name } } } }";

/// Comments on issues inside the row's configured scope.
pub const COMMENTS_QUERY: &str =
    "query BoughComments($after: String, $first: Int!, $filter: CommentFilter) { \
comments(first: $first, after: $after, filter: $filter) { \
pageInfo { hasNextPage endCursor } \
nodes { id body url createdAt updatedAt user { name } issue { identifier title url } } } }";

/// The two sources, each with its own watermark and its own query.
pub const SOURCES: [&str; 2] = ["issues", "comments"];

/// PURE: the query one source sends.
pub fn query_for(source: &str) -> &'static str {
    match source {
        "issues" => ISSUES_QUERY,
        _ => COMMENTS_QUERY,
    }
}

/// PURE: the `$filter` variable one source sends, from the row's `teams` and `projects`.
///
/// `issues` additionally pins `assignee.isMe`, because `WakeClass::Assigned` is what the sweep
/// stamps on every issue it returns — the class has to be true by construction of the query, not
/// asserted by the config.
///
/// Returns `None` for "no filter", which the caller sends as a JSON `null`: a legal GraphQL
/// value for an optional argument. Only reachable for `comments` with no scope at all, which the
/// row refuses to sweep anyway.
pub fn filter_for(
    source: &str,
    teams: &[String],
    projects: &[String],
) -> Option<serde_json::Value> {
    let mut and: Vec<serde_json::Value> = Vec::new();
    let (team_path, project_path) = match source {
        // A comment is scoped through the issue it hangs off.
        "comments" => ("issue", "issue"),
        _ => ("", ""),
    };
    let nest = |path: &str, inner: serde_json::Value| -> serde_json::Value {
        if path.is_empty() {
            inner
        } else {
            serde_json::json!({ path: inner })
        }
    };
    if source == "issues" {
        and.push(serde_json::json!({ "assignee": { "isMe": { "eq": true } } }));
    }
    if !teams.is_empty() {
        and.push(nest(
            team_path,
            serde_json::json!({ "team": { "key": { "in": teams } } }),
        ));
    }
    if !projects.is_empty() {
        and.push(nest(
            project_path,
            serde_json::json!({ "project": { "name": { "in": projects } } }),
        ));
    }
    if and.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "and": and }))
}

/// PURE: `{ data: { <field>: { nodes: [...], pageInfo: { endCursor } } } }` → its nodes and its
/// cursor. A payload that is not that shape is `None`, which the sweep reports as a failed source.
pub fn page(
    source: &str,
    value: &serde_json::Value,
) -> Option<(Vec<serde_json::Value>, Option<String>)> {
    let field = value.get("data")?.get(source)?;
    let nodes = field.get("nodes")?.as_array()?.clone();
    let cursor = field
        .get("pageInfo")
        .and_then(|p| p.get("endCursor"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    Some((nodes, cursor))
}

fn ts(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn str_of<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    node.get(key).and_then(|v| v.as_str())
}

/// PURE: one issue node becomes a [`Collected`] carrying `linear:TEAM-123`.
pub fn issue_of(node: &serde_json::Value) -> Option<Collected> {
    let key = str_of(node, "identifier")?;
    let at = ts(str_of(node, "updatedAt"))?;
    let title = str_of(node, "title").unwrap_or("").to_string();
    let state = node
        .get("state")
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let assignee = node
        .get("assignee")
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let url = str_of(node, "url").map(|s| s.to_string());
    let description = str_of(node, "description").unwrap_or("");
    let r = refs::issue(key);
    Some(Collected {
        subject: format!("{key} {title}"),
        summary: format!("{state}, assigned to {assignee}"),
        text: format!(
            "{key} {title}\n{state}, assigned to {assignee}\n{}\n\n{description}",
            url.clone().unwrap_or_default()
        ),
        refs: BTreeSet::from([r.clone(), refs::team(key)]),
        r#ref: r,
        url,
        // Overwritten at the sweep from the row's configured `wake_classes`.
        class: MailClass::Ordinary,
        at,
        order: at.timestamp_millis(),
    })
}

/// PURE: one comment node becomes a [`Collected`] carrying `linear:TEAM-123:comment:<id>`, and
/// its issue's ref for the router.
pub fn comment_of(node: &serde_json::Value) -> Option<Collected> {
    let id = str_of(node, "id")?;
    let issue = node.get("issue")?;
    let key = str_of(issue, "identifier")?;
    let at = ts(str_of(node, "updatedAt")).or_else(|| ts(str_of(node, "createdAt")))?;
    let body = str_of(node, "body").unwrap_or("").to_string();
    let user = node
        .get("user")
        .and_then(|u| u.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let title = str_of(issue, "title").unwrap_or("");
    let url = str_of(node, "url")
        .or_else(|| str_of(issue, "url"))
        .map(|s| s.to_string());
    let r = refs::issue_comment(key, id);
    Some(Collected {
        subject: format!("{key} comment from {user}"),
        summary: body.lines().next().unwrap_or("").trim().to_string(),
        text: format!("{key} {title}\ncomment from {user}\n\n{body}"),
        refs: BTreeSet::from([r.clone(), refs::issue(key), refs::team(key)]),
        r#ref: r,
        url,
        class: MailClass::Ordinary,
        at,
        order: at.timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issue_node_becomes_a_cited_item() {
        let node = serde_json::json!({
            "id": "uuid", "identifier": "TEAM-123", "title": "a ticket",
            "url": "https://linear.invalid/TEAM-123", "updatedAt": "2026-08-01T00:00:00Z",
            "description": "do the thing", "assignee": { "name": "Andrey" },
            "state": { "name": "In Progress" }
        });
        let c = issue_of(&node).expect("an issue");
        assert_eq!(c.r#ref.as_str(), "linear:TEAM-123");
        assert_eq!(c.order, c.at.timestamp_millis());
        assert!(c.summary.contains("In Progress"));
    }

    #[test]
    fn a_comment_carries_its_issue_ref_for_the_router() {
        let node = serde_json::json!({
            "id": "c1", "body": "a note\nmore", "updatedAt": "2026-08-01T00:00:00Z",
            "user": { "name": "A Teammate" },
            "issue": { "identifier": "TEAM-123", "title": "a ticket", "url": "https://linear.invalid/TEAM-123" }
        });
        let c = comment_of(&node).expect("a comment");
        assert_eq!(c.r#ref.as_str(), "linear:TEAM-123:comment:c1");
        assert!(c.refs.contains(&refs::issue("TEAM-123")));
        assert_eq!(c.summary, "a note");
    }

    #[test]
    fn a_node_without_an_identifier_is_unusable_rather_than_invented() {
        assert!(issue_of(&serde_json::json!({ "updatedAt": "2026-08-01T00:00:00Z" })).is_none());
        assert!(comment_of(&serde_json::json!({ "id": "c1" })).is_none());
    }

    #[test]
    fn a_page_yields_its_nodes_and_its_cursor() {
        let value = serde_json::json!({
            "data": { "issues": { "nodes": [{}], "pageInfo": { "endCursor": "abc" } } }
        });
        let (nodes, cursor) = page("issues", &value).expect("a page");
        assert_eq!(nodes.len(), 1);
        assert_eq!(cursor.as_deref(), Some("abc"));
        assert!(page("issues", &serde_json::json!({ "errors": [] })).is_none());
    }

    #[test]
    fn the_issue_filter_pins_the_assignee_and_the_configured_scope() {
        let f = filter_for("issues", &["ENG".into()], &["Rebuild".into()]).expect("a filter");
        let and = f["and"].as_array().expect("an `and` list");
        assert_eq!(and.len(), 3, "{f}");
        assert_eq!(
            and[0],
            serde_json::json!({ "assignee": { "isMe": { "eq": true } } })
        );
        assert_eq!(and[1]["team"]["key"]["in"][0], "ENG");
        assert_eq!(and[2]["project"]["name"]["in"][0], "Rebuild");
    }

    /// An unscoped `issues` sweep is still pinned to the viewer: `WakeClass::Assigned` is stamped
    /// on everything this query returns, so the query has to be the thing that makes it true.
    #[test]
    fn an_unscoped_issue_filter_still_pins_the_assignee() {
        let f = filter_for("issues", &[], &[]).expect("a filter");
        assert_eq!(f["and"].as_array().unwrap().len(), 1);
        assert_eq!(f["and"][0]["assignee"]["isMe"]["eq"], true);
    }

    /// A comment is scoped through its issue, and an unscoped one has no filter at all.
    #[test]
    fn a_comment_filter_reaches_through_the_issue() {
        let f = filter_for("comments", &["ENG".into()], &[]).expect("a filter");
        assert_eq!(f["and"][0]["issue"]["team"]["key"]["in"][0], "ENG");
        assert_eq!(filter_for("comments", &[], &[]), None);
    }

    #[test]
    fn each_source_sends_its_own_named_query() {
        assert!(query_for("issues").contains("BoughIssues"));
        assert!(query_for("comments").contains("BoughComments"));
        assert!(query_for("issues").contains("$filter: IssueFilter"));
        assert!(query_for("comments").contains("$filter: CommentFilter"));
    }
}
