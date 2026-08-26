//! §7's boundary as the Provider enforces it: a push only onto Andrey's own OPEN pull request, a
//! thread op only on a BOT thread, and uncertainty counted as human. Every case is a LOOKUP
//! against the world made before anything is written — asserted by reading the fake's log.

mod support;

use std::sync::Arc;

use bough_plugin_actions::{ActionKind, ActionProvider};
use support::*;

fn pr_view(author: &str, state: &str) -> serde_json::Value {
    serde_json::json!({
        "author": { "login": author },
        "state": state,
        "isDraft": false,
        "headRefName": "feature",
    })
}

fn comment_by(login: &str, account_type: &str) -> serde_json::Value {
    serde_json::json!({ "user": { "login": login, "type": account_type } })
}

fn push_payload() -> serde_json::Value {
    serde_json::json!({ "branch": "feature", "commits": ["abc123"] })
}

#[tokio::test]
async fn push_to_pr_refuses_a_pr_authored_by_someone_else() {
    let gh = Arc::new(FakeGh::new("andrey").read("pr view", pr_view("teammate", "OPEN")));
    let p = provider(&gh);
    let e = p
        .execute(&exec(ActionKind::PushToPr, "owner/repo#12", push_payload()))
        .await
        .expect_err("a teammate's branch is never pushed to");
    let text = refusal(e);
    assert!(text.contains("authored by `teammate`"), "{text}");
    assert!(text.contains("never teammates' branches"), "{text}");
    assert!(
        gh.log().iter().all(|c| !c.write),
        "the refusal happened before anything was written: {:?}",
        gh.log()
    );
}

#[tokio::test]
async fn push_to_pr_refuses_a_closed_pr() {
    let gh = Arc::new(FakeGh::new("andrey").read("pr view", pr_view("andrey", "CLOSED")));
    let p = provider(&gh);
    let e = p
        .execute(&exec(ActionKind::PushToPr, "owner/repo#12", push_payload()))
        .await
        .expect_err("a closed PR is not pushed to");
    let text = refusal(e);
    assert!(text.contains("is CLOSED, not open"), "{text}");
    assert!(gh.log().iter().all(|c| !c.write), "nothing was written");
}

#[tokio::test]
async fn push_to_pr_pushes_the_commit_that_carries_the_marker_and_refuses_one_that_does_not() {
    let req = exec(ActionKind::PushToPr, "owner/repo#12", push_payload());
    let marked = bough_plugin_actions_github::marker::commit_trailer("do the thing", &req.marker);

    // Without the trailer the push is refused: its artifact could never be reconciled.
    let bare = Arc::new(
        FakeGh::new("andrey")
            .read("pr view", pr_view("andrey", "OPEN"))
            .read(
                "commits/abc123",
                serde_json::json!({ "commit": { "message": "do the thing" } }),
            ),
    );
    let e = provider(&bare)
        .execute(&req)
        .await
        .expect_err("an unmarked commit is not pushed");
    assert!(refusal(e).contains("carries no `Bough-Action:"));
    assert!(bare.log().iter().all(|c| !c.write));

    // With it, one write moves the PR's head ref and the artifact is the marked commit.
    let ok = Arc::new(
        FakeGh::new("andrey")
            .read("pr view", pr_view("andrey", "OPEN"))
            .read(
                "commits/abc123",
                serde_json::json!({ "commit": { "message": marked } }),
            ),
    );
    let artifact = provider(&ok).execute(&req).await.expect("the push happens");
    assert_eq!(artifact.locator, "abc123");
    assert_eq!(artifact.marker, req.marker);
    let w = last_write(&ok);
    assert_eq!(w.argv[0], "api");
    assert!(
        w.argv
            .iter()
            .any(|a| a == "repos/owner/repo/git/refs/heads/feature"),
        "{:?}",
        w.argv
    );
    assert!(w.argv.iter().any(|a| a == "sha=abc123"), "{:?}", w.argv);
}

#[tokio::test]
async fn bot_thread_op_resolves_a_bot_typed_thread() {
    let gh = Arc::new(
        FakeGh::new("andrey")
            .read("pulls/comments/77", comment_by("some-linter", "Bot"))
            .stdout(r#"{"html_url":"https://github.com/owner/repo/pull/12#discussion_r99"}"#),
    );
    let req = exec(
        ActionKind::BotThreadOp,
        "owner/repo#12",
        serde_json::json!({ "thread": "77", "op": "resolve", "body": null }),
    );
    let artifact = provider(&gh)
        .execute(&req)
        .await
        .expect("a bot thread resolves");
    assert_eq!(
        artifact.locator,
        "https://github.com/owner/repo/pull/12#discussion_r99"
    );
    let writes: Vec<_> = gh.log().into_iter().filter(|c| c.write).collect();
    assert_eq!(writes.len(), 2, "one comment, then the resolve: {writes:?}");
    let body = writes[0]
        .argv
        .iter()
        .find(|a| a.starts_with("body="))
        .expect("the comment carries a body");
    assert!(
        body.ends_with(&format!("<!-- {} -->", req.marker)),
        "the comment's LAST line is the marker: {body}"
    );
    assert!(
        writes[1]
            .argv
            .iter()
            .any(|a| a.contains("resolveReviewThread")),
        "{:?}",
        writes[1].argv
    );
}

#[tokio::test]
async fn bot_thread_op_refuses_a_human_thread() {
    let gh =
        Arc::new(FakeGh::new("andrey").read("pulls/comments/77", comment_by("a-teammate", "User")));
    let e = provider(&gh)
        .execute(&exec(
            ActionKind::BotThreadOp,
            "owner/repo#12",
            serde_json::json!({ "thread": "77", "op": "resolve", "body": null }),
        ))
        .await
        .expect_err("a human thread is never auto-resolved");
    let text = refusal(e);
    assert!(text.contains("opened by `a-teammate`"), "{text}");
    assert!(text.contains("never auto-resolved"), "{text}");
    assert!(gh.log().iter().all(|c| !c.write), "nothing was written");
}

/// UNCERTAIN IS HUMAN: an author whose account type GitHub returns empty is refused, and the
/// refusal SAYS it was uncertain rather than pretending to a verdict.
#[tokio::test]
async fn bot_thread_op_refuses_an_uncertain_thread_as_human() {
    let gh = Arc::new(FakeGh::new("andrey").read("pulls/comments/77", comment_by("mystery", "")));
    let e = provider(&gh)
        .execute(&exec(
            ActionKind::BotThreadOp,
            "owner/repo#12",
            serde_json::json!({ "thread": "77", "op": "resolve", "body": null }),
        ))
        .await
        .expect_err("uncertain is human");
    let text = refusal(e);
    assert!(text.contains("uncertain"), "{text}");
    assert!(text.contains("never auto-resolved"), "{text}");
    assert!(gh.log().iter().all(|c| !c.write));
}

/// An allowlisted login is a bot even when the account type does not say so.
#[tokio::test]
async fn an_allowlisted_login_is_a_bot_thread() {
    let gh = Arc::new(
        FakeGh::new("andrey")
            .read("pulls/comments/77", comment_by("dependabot[bot]", "User"))
            .stdout("{}"),
    );
    provider(&gh)
        .execute(&exec(
            ActionKind::BotThreadOp,
            "owner/repo#12",
            serde_json::json!({ "thread": "77", "op": "reply", "body": "on it" }),
        ))
        .await
        .expect("the allowlist makes it a bot thread");
}

#[tokio::test]
async fn open_pr_puts_the_marker_on_the_last_line_of_the_body() {
    let gh = Arc::new(FakeGh::new("andrey").stdout("https://github.com/owner/repo/pull/99\n"));
    let req = exec(
        ActionKind::OpenPr,
        "owner/repo",
        serde_json::json!({ "head": "feature", "base": "main", "title": "a title", "body": "why" }),
    );
    let artifact = provider(&gh).execute(&req).await.expect("the PR opens");
    assert_eq!(artifact.locator, "https://github.com/owner/repo/pull/99");
    let w = last_write(&gh);
    let body = w.argv[w.argv.iter().position(|a| a == "--body").unwrap() + 1].clone();
    assert!(body.starts_with("why"));
    assert_eq!(
        body.lines().last().unwrap(),
        format!("<!-- {} -->", req.marker),
        "the marker is the LAST line of the PR body"
    );
    assert_eq!(
        artifact.marker, req.marker,
        "the artifact reports the marker it embedded"
    );
}

/// The marker is DERIVED from the idem key, so reconciliation can recompute it from the journal.
#[tokio::test]
async fn the_marker_in_the_pr_body_is_derived_from_the_idem_key() {
    let req = exec(
        ActionKind::OpenPr,
        "owner/repo",
        serde_json::json!({ "head": "f", "base": "main", "title": "t", "body": "b" }),
    );
    assert_eq!(
        req.marker,
        format!("bough-action:{}", &req.idem_key.as_str()[..16])
    );
}
