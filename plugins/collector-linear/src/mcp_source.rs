//! Invariant: the MCP transport speaks the `linear-server` MCP vocabulary (`list_issues`,
//! `list_comments`) and parsing is PURE, so the whole path is testable against a stub
//! [`bough_plugin_mcp::McpClient`] and recorded payloads. Tool names and argument shapes are
//! protocol details of that server, CONSTANTS in this file, not deployment knobs (§0.2).
//!
//! The scope semantics differ from the GraphQL transport in one honest way, written down here so
//! nobody discovers it in production: `list_comments` is per-issue only, so the `comments` source
//! sweeps comments ON THE VIEWER'S ASSIGNED ISSUES inside the scope (the issues whose `updatedAt`
//! moved, which a new comment does), where GraphQL swept every comment in the scope. And the MCP
//! `team` argument takes a team NAME or ID where GraphQL took the key; the row's `teams` values
//! are passed verbatim as that argument.

use std::collections::BTreeSet;

use bough_plugin_agents::MailClass;
use bough_plugin_collect_core::{refs, Collected};
use bough_plugin_mcp::McpCallResult;
use chrono::{DateTime, Utc};

/// The two tools this transport calls, by the names `linear-server` registers them under.
pub const ISSUES_TOOL: &str = "list_issues";
pub const COMMENTS_TOOL: &str = "list_comments";

/// The issue fields every call asks for: everything [`mcp_issue_of`] reads, nothing more.
const ISSUE_FIELDS: [&str; 7] = [
    "id",
    "title",
    "url",
    "updatedAt",
    "status",
    "assignee",
    "description",
];

/// One scope unit: each configured team and each configured project is its own call, because the
/// MCP `team` / `project` arguments take one value where GraphQL's filter took a list.
pub fn scopes(teams: &[String], projects: &[String]) -> Vec<(&'static str, String)> {
    teams
        .iter()
        .map(|t| ("team", t.clone()))
        .chain(projects.iter().map(|p| ("project", p.clone())))
        .collect()
}

/// PURE: the `list_issues` arguments for one scope unit. `assignee: "me"` is pinned for the same
/// reason the GraphQL filter pins `assignee.isMe`: `WakeClass::Assigned` is stamped on everything
/// this returns, so the call has to be what makes it true.
pub fn issues_args(
    scope: &(&'static str, String),
    after: Option<DateTime<Utc>>,
    batch: usize,
) -> serde_json::Value {
    let mut args = serde_json::json!({
        "assignee": "me",
        "orderBy": "updatedAt",
        "limit": batch,
        "fields": ISSUE_FIELDS,
    });
    args[scope.0] = serde_json::Value::String(scope.1.clone());
    if let Some(at) = after {
        args["updatedAt"] = serde_json::Value::String(at.to_rfc3339());
    }
    args
}

/// PURE: the `list_comments` arguments for one issue.
pub fn comments_args(issue_key: &str, batch: usize) -> serde_json::Value {
    serde_json::json!({
        "issueId": issue_key,
        "orderBy": "updatedAt",
        "limit": batch,
    })
}

/// PURE: a call result's payload as JSON. The server answers with one text block holding a JSON
/// object; a structured result is preferred when the transport carried one.
pub fn payload(result: &McpCallResult) -> Result<serde_json::Value, String> {
    if let Some(v) = &result.value {
        return Ok(v.clone());
    }
    serde_json::from_str(&result.content)
        .map_err(|e| format!("unparseable payload ({} bytes): {e}", result.content.len()))
}

/// PURE: `{ "<field>": [...] }` → its nodes. A payload that is not that shape is an error the
/// sweep reports as a failed source, never a silent empty sweep.
pub fn nodes_of(value: &serde_json::Value, field: &str) -> Result<Vec<serde_json::Value>, String> {
    value
        .get(field)
        .and_then(|n| n.as_array())
        .cloned()
        .ok_or_else(|| format!("payload has no `{field}` array"))
}

fn ts(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn str_of<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    node.get(key).and_then(|v| v.as_str())
}

/// PURE: one MCP issue node becomes the same [`Collected`] the GraphQL transport produces, so the
/// dedupe guard and the router cannot tell the transports apart. The MCP shape is FLAT: `id` is
/// the identifier (`TEAM-123`), `status` and `assignee` are strings.
pub fn mcp_issue_of(node: &serde_json::Value) -> Option<Collected> {
    let key = str_of(node, "id")?;
    let at = ts(str_of(node, "updatedAt"))?;
    let title = str_of(node, "title").unwrap_or("").to_string();
    let state = str_of(node, "status").unwrap_or("");
    let assignee = str_of(node, "assignee").unwrap_or("");
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

/// What [`mcp_comment_of`] needs to know about the issue a comment hangs off, because the MCP
/// comment node does not carry its issue (the call was already per-issue).
pub struct IssueMeta {
    pub key: String,
    pub title: String,
    pub url: Option<String>,
}

/// PURE: the issue meta the comments pass reads off an issue node.
pub fn issue_meta(node: &serde_json::Value) -> Option<IssueMeta> {
    Some(IssueMeta {
        key: str_of(node, "id")?.to_string(),
        title: str_of(node, "title").unwrap_or("").to_string(),
        url: str_of(node, "url").map(|s| s.to_string()),
    })
}

/// PURE: one MCP comment node becomes the same [`Collected`] the GraphQL transport produces.
pub fn mcp_comment_of(node: &serde_json::Value, issue: &IssueMeta) -> Option<Collected> {
    let id = str_of(node, "id")?;
    let at = ts(str_of(node, "updatedAt")).or_else(|| ts(str_of(node, "createdAt")))?;
    let body = str_of(node, "body").unwrap_or("").to_string();
    let user = node
        .get("author")
        .and_then(|u| u.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    let key = issue.key.as_str();
    let r = refs::issue_comment(key, id);
    Some(Collected {
        subject: format!("{key} comment from {user}"),
        summary: body.lines().next().unwrap_or("").trim().to_string(),
        text: format!("{key} {}\ncomment from {user}\n\n{body}", issue.title),
        refs: BTreeSet::from([r.clone(), refs::issue(key), refs::team(key)]),
        r#ref: r,
        url: issue.url.clone(),
        class: MailClass::Ordinary,
        at,
        order: at.timestamp_millis(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `linear-server` actually answers with, recorded 2026-08-29.
    fn issues_payload() -> &'static str {
        r#"{"issues":[{"id":"NME-1668","title":"Switch frontend log reads to the new service","url":"https://linear.app/x/issue/NME-1668/slug","updatedAt":"2026-08-28T16:25:57.956Z","status":"Todo","assignee":"Andrey Lukin"}],"hasNextPage":true,"cursor":"e161929e"}"#
    }

    fn comments_payload() -> &'static str {
        r#"{"comments":[{"id":"8f39092d-7461","body":"a survey\nmore","createdAt":"2026-08-12T11:05:30.430Z","updatedAt":"2026-08-12T11:05:30.401Z","author":{"id":"u1","name":"Andrey Lukin"},"onBehalfOf":null}],"hasNextPage":false}"#
    }

    fn text_result(content: &str) -> McpCallResult {
        McpCallResult {
            content: content.to_string(),
            value: None,
            cites: Vec::new(),
            is_error: false,
        }
    }

    #[test]
    fn a_recorded_issues_payload_becomes_cited_items() {
        let value = payload(&text_result(issues_payload())).expect("json");
        let nodes = nodes_of(&value, "issues").expect("nodes");
        let c = mcp_issue_of(&nodes[0]).expect("an issue");
        assert_eq!(c.r#ref.as_str(), "linear:NME-1668");
        assert!(c.refs.contains(&refs::team("NME-1668")));
        assert!(c.summary.contains("Todo"));
        assert_eq!(c.order, c.at.timestamp_millis());
    }

    #[test]
    fn a_recorded_comments_payload_becomes_cited_items_carrying_the_issue_ref() {
        let value = payload(&text_result(comments_payload())).expect("json");
        let nodes = nodes_of(&value, "comments").expect("nodes");
        let issue = IssueMeta {
            key: "NME-1673".into(),
            title: "a ticket".into(),
            url: Some("https://linear.app/x/issue/NME-1673".into()),
        };
        let c = mcp_comment_of(&nodes[0], &issue).expect("a comment");
        assert_eq!(c.r#ref.as_str(), "linear:NME-1673:comment:8f39092d-7461");
        assert!(c.refs.contains(&refs::issue("NME-1673")));
        assert_eq!(c.summary, "a survey");
        assert_eq!(
            c.url.as_deref(),
            Some("https://linear.app/x/issue/NME-1673")
        );
    }

    #[test]
    fn a_payload_that_is_not_the_expected_shape_is_an_error_never_an_empty_sweep() {
        assert!(payload(&text_result("not json")).is_err());
        let value = payload(&text_result(r#"{"nope":[]}"#)).expect("json");
        assert!(nodes_of(&value, "issues").unwrap_err().contains("issues"));
    }

    #[test]
    fn the_issue_call_pins_the_viewer_and_carries_the_scope_and_the_watermark() {
        let after = ts(Some("2026-08-01T00:00:00Z"));
        let args = issues_args(&("team", "FOMS".to_string()), after, 50);
        assert_eq!(args["assignee"], "me");
        assert_eq!(args["team"], "FOMS");
        assert_eq!(args["orderBy"], "updatedAt");
        assert_eq!(args["limit"], 50);
        assert!(args["updatedAt"]
            .as_str()
            .unwrap()
            .starts_with("2026-08-01"));

        let args = issues_args(&("project", "Rebuild".to_string()), None, 10);
        assert_eq!(args["project"], "Rebuild");
        assert!(args.get("updatedAt").is_none());
        assert!(args.get("team").is_none());
    }

    #[test]
    fn each_team_and_each_project_is_its_own_scope_unit() {
        let s = scopes(&["FOMS".into(), "OPS".into()], &["Rebuild".into()]);
        assert_eq!(
            s,
            vec![
                ("team", "FOMS".to_string()),
                ("team", "OPS".to_string()),
                ("project", "Rebuild".to_string()),
            ]
        );
    }
}
