//! Invariant: parsing is PURE and NEVER SILENT. The Slack MCP server answers a search with one
//! text block: a JSON object whose `results` field is rendered markdown, one `### Result` block
//! per message with `Channel: … (ID: X)`, `Message_ts:` and `Text:` lines (recorded 2026-08-29,
//! `response_format: "detailed"`). That rendering is a third-party surface that can drift, so the
//! rule here is: a payload that contains result blocks none of which parse is an ERROR the sweep
//! reports, never an empty sweep that reads as "nothing new".

use std::collections::BTreeSet;

use bough_plugin_agents::MailClass;
use bough_plugin_collect_core::{refs, Collected};
use bough_plugin_mcp::McpCallResult;
use chrono::{DateTime, Utc};

/// The one tool this collector calls, by the name the Slack MCP server registers it under.
pub const SEARCH_TOOL: &str = "slack_search_public_and_private";

/// The server caps a search page at 20; a larger configured batch still asks for 20.
pub const MAX_PAGE: usize = 20;

/// PURE: the search arguments for one query. Ascending by timestamp so the watermark advances
/// monotonically; `after` is the watermark in epoch SECONDS (the server treats it as inclusive,
/// and the ref guard eats the boundary duplicate).
pub fn search_args(query: &str, after_secs: Option<i64>, batch: usize) -> serde_json::Value {
    let mut args = serde_json::json!({
        "query": query,
        "sort": "timestamp",
        "sort_dir": "asc",
        "limit": batch.min(MAX_PAGE),
        "include_context": false,
        "response_format": "detailed",
    });
    if let Some(secs) = after_secs {
        args["after"] = serde_json::Value::String(secs.to_string());
    }
    args
}

/// PURE: the rendered markdown out of a call result. The content is a JSON object with a
/// `results` string; a structured result is preferred when the transport carried one, and a
/// content that is not that shape is used verbatim (the rendering, unwrapped).
pub fn results_text(result: &McpCallResult) -> String {
    let value: Option<serde_json::Value> = result
        .value
        .clone()
        .or_else(|| serde_json::from_str(&result.content).ok());
    match value
        .as_ref()
        .and_then(|v| v.get("results"))
        .and_then(|r| r.as_str())
    {
        Some(text) => text.to_string(),
        None => result.content.clone(),
    }
}

/// PURE: `1787946300.879929` → microseconds since the epoch, Slack's own message identity made
/// orderable. The fractional part is padded/clipped to 6 digits.
pub fn ts_micros(ts: &str) -> Option<i64> {
    let (secs, frac) = match ts.split_once('.') {
        Some((s, f)) => (s, f),
        None => (ts, ""),
    };
    let secs: i64 = secs.parse().ok()?;
    let mut frac = frac.to_string();
    frac.truncate(6);
    while frac.len() < 6 {
        frac.push('0');
    }
    let micros: i64 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    Some(secs.checked_mul(1_000_000)? + micros)
}

/// One parsed result block, before it becomes a [`Collected`].
#[derive(Debug, Default)]
struct Block {
    channel_label: String,
    channel_id: String,
    from: String,
    ts: String,
    permalink: Option<String>,
    text: Vec<String>,
}

/// The value inside the LAST `(ID: …)` on a line: `Channel: #eng (ID: C0CCC)` → `C0CCC`.
fn id_of(line: &str) -> Option<String> {
    let start = line.rfind("(ID: ")?;
    let rest = &line[start + 5..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().to_string())
}

/// PURE: the rendered search results, parsed. `Ok` items carry `slack:<channel>:<ts>` refs; a
/// text that HAS result blocks but yields nothing is an `Err` naming the drift.
pub fn messages_of(text: &str) -> Result<Vec<Collected>, String> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut current: Option<Block> = None;
    let mut in_text = false;
    for line in text.lines() {
        if line.starts_with("### Result") {
            if let Some(b) = current.take() {
                blocks.push(b);
            }
            current = Some(Block::default());
            in_text = false;
            continue;
        }
        let Some(b) = current.as_mut() else { continue };
        if in_text {
            if line.trim() == "---" {
                in_text = false;
                blocks.push(current.take().unwrap_or_default());
            } else {
                b.text.push(line.to_string());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("Channel:") {
            b.channel_id = id_of(rest).unwrap_or_default();
            b.channel_label = rest.split(" (ID:").next().unwrap_or("").trim().to_string();
        } else if let Some(rest) = line.strip_prefix("From:") {
            b.from = rest
                .split(['<', '('])
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
        } else if let Some(rest) = line.strip_prefix("Message_ts:") {
            b.ts = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("Permalink:") {
            b.permalink = rest
                .split_once('(')
                .and_then(|(_, tail)| tail.split(')').next())
                .map(|u| u.trim().to_string());
        } else if line.starts_with("Text:") {
            in_text = true;
        }
    }
    if let Some(b) = current.take() {
        blocks.push(b);
    }

    let seen = blocks.len();
    let items: Vec<Collected> = blocks.into_iter().filter_map(collected_of).collect();
    if items.is_empty() && seen > 0 {
        return Err(format!(
            "{seen} result block(s) and none parsed: the server's rendering has drifted from \
             what parse.rs reads (Channel/Message_ts/Text lines)"
        ));
    }
    Ok(items)
}

/// One block becomes a [`Collected`], or `None` when it lacks an identity (channel + ts).
fn collected_of(b: Block) -> Option<Collected> {
    if b.channel_id.is_empty() {
        return None;
    }
    let order = ts_micros(&b.ts)?;
    let at: DateTime<Utc> = DateTime::from_timestamp(order.div_euclid(1_000_000), 0)?;
    let body = b.text.join("\n").trim().to_string();
    let place = if b.channel_label.is_empty() {
        b.channel_id.clone()
    } else {
        b.channel_label.clone()
    };
    let r = refs::slack_message(&b.channel_id, &b.ts);
    Some(Collected {
        subject: format!("{place} from {}", b.from),
        summary: body.lines().next().unwrap_or("").trim().to_string(),
        text: format!(
            "{place} from {}\n{}\n\n{body}",
            b.from,
            b.permalink.clone().unwrap_or_default()
        ),
        refs: BTreeSet::from([r.clone(), refs::channel(&b.channel_id)]),
        r#ref: r,
        url: b.permalink,
        // Overwritten at the sweep from the row's configured `wake_classes`.
        class: MailClass::Ordinary,
        at,
        order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = include_str!("../../../scripts/fixtures/slack/mcp-search.json");

    fn text_result(content: &str) -> McpCallResult {
        McpCallResult {
            content: content.to_string(),
            value: None,
            cites: Vec::new(),
            is_error: false,
        }
    }

    #[test]
    fn the_recorded_payload_becomes_cited_items_ordered_by_ts() {
        let text = results_text(&text_result(RECORDED));
        let items = messages_of(&text).expect("parsed");
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].r#ref.as_str(),
            "slack:D0AAA11AAAA:1787946300.879929"
        );
        assert!(items[0].refs.contains(&refs::channel("D0AAA11AAAA")));
        assert_eq!(items[0].order, 1_787_946_300_879_929);
        assert_eq!(items[0].subject, "DM from A Teammate");
        assert!(items[0].summary.starts_with("Saving for our convo"));
        assert!(items[0]
            .url
            .as_deref()
            .unwrap()
            .starts_with("https://example.enterprise.slack.com/archives/"));

        assert_eq!(items[1].subject, "#eng from Someone Else");
        assert_eq!(items[1].summary, "could you take a look at the PR?");
        assert!(items[1].text.contains("second line of the ask"));
    }

    #[test]
    fn no_results_is_empty_and_ok_but_unparseable_blocks_are_an_error() {
        let empty = "# Search Results for: to:me\n\n## Messages (0 results)\n";
        assert!(messages_of(empty).expect("empty is fine").is_empty());

        let drifted = "### Result 1 of 1\nSomething: entirely different\n";
        let err = messages_of(drifted).expect_err("loud");
        assert!(err.contains("drifted"), "{err}");
    }

    #[test]
    fn a_ts_is_slacks_own_identity_made_orderable() {
        assert_eq!(ts_micros("1787946300.879929"), Some(1_787_946_300_879_929));
        assert_eq!(ts_micros("1787946300"), Some(1_787_946_300_000_000));
        assert_eq!(ts_micros("1787946300.8799291"), Some(1_787_946_300_879_929));
        assert_eq!(ts_micros("not a ts"), None);
    }

    #[test]
    fn the_search_call_is_ascending_bounded_and_detailed() {
        let args = search_args("to:me", Some(1_787_946_300), 50);
        assert_eq!(args["sort"], "timestamp");
        assert_eq!(args["sort_dir"], "asc");
        assert_eq!(args["limit"], 20, "the server caps a page at 20");
        assert_eq!(args["after"], "1787946300");
        assert_eq!(args["response_format"], "detailed");
        assert!(search_args("to:me", None, 5).get("after").is_none());
    }

    #[test]
    fn a_bare_markdown_content_without_the_json_wrapper_still_reads() {
        let raw = "### Result 1 of 1\nChannel: #x (ID: C1)\nMessage_ts: 5.0\nText: \nhi\n\n---\n";
        let text = results_text(&text_result(raw));
        assert_eq!(messages_of(&text).expect("parsed").len(), 1);
    }
}
