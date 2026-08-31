//! Invariant: interpreting a hook's answer is PURE, and it can only TIGHTEN — a verdict maps to
//! deny/ask/block/attach on the tools waterfalls, whose guard is monotone by type; nothing here
//! can widen a decision. A hook that errors or times out changes NOTHING (both CLIs treat a
//! failing hook as non-blocking) and is warned about by the caller.

use crate::run::HookRun;

/// What a `PreToolUse` hook decided.
#[derive(Clone, Debug, PartialEq)]
pub enum PreVerdict {
    Deny(String),
    Ask(String),
    Nothing,
}

/// What a `PostToolUse` hook decided.
#[derive(Clone, Debug, PartialEq)]
pub struct PostOut {
    pub block: Option<String>,
    pub context: Option<String>,
}

/// Both CLIs' contract: exit `2` blocks with stderr as the reason; exit `0` may carry a JSON
/// object on stdout (`hookSpecificOutput.permissionDecision` deny/ask/allow, or the legacy
/// top-level `{"decision": "block", "reason": ...}`); anything else decides nothing.
pub fn pre_verdict(run: &HookRun) -> PreVerdict {
    if run.timed_out {
        return PreVerdict::Nothing;
    }
    match run.status {
        Some(2) => PreVerdict::Deny(reason_or(&run.stderr, "blocked by hook")),
        Some(0) => {
            let Some(v) = stdout_json(run) else {
                return PreVerdict::Nothing;
            };
            let hso = v.get("hookSpecificOutput");
            let decision = hso
                .and_then(|h| h.get("permissionDecision"))
                .and_then(|d| d.as_str());
            let hso_reason = || {
                hso.and_then(|h| h.get("permissionDecisionReason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            match decision {
                Some("deny") => return PreVerdict::Deny(reason_or(&hso_reason(), "denied by hook")),
                Some("ask") => return PreVerdict::Ask(reason_or(&hso_reason(), "hook asks")),
                Some(_) => return PreVerdict::Nothing,
                None => {}
            }
            if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
                let r = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
                return PreVerdict::Deny(reason_or(r, "blocked by hook"));
            }
            PreVerdict::Nothing
        }
        _ => PreVerdict::Nothing,
    }
}

/// `PostToolUse`: exit `2` blocks with stderr; JSON `{"decision": "block", "reason"}` blocks;
/// `hookSpecificOutput.additionalContext` attaches context either way.
pub fn post_out(run: &HookRun) -> PostOut {
    let mut out = PostOut {
        block: None,
        context: None,
    };
    if run.timed_out {
        return out;
    }
    match run.status {
        Some(2) => out.block = Some(reason_or(&run.stderr, "blocked by hook")),
        Some(0) => {
            if let Some(v) = stdout_json(run) {
                if v.get("decision").and_then(|d| d.as_str()) == Some("block") {
                    let r = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
                    out.block = Some(reason_or(r, "blocked by hook"));
                }
                out.context = v
                    .get("hookSpecificOutput")
                    .and_then(|h| h.get("additionalContext"))
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .map(str::to_string);
            }
        }
        _ => {}
    }
    out
}

fn stdout_json(run: &HookRun) -> Option<serde_json::Value> {
    serde_json::from_str(run.stdout.trim()).ok()
}

fn reason_or(s: &str, fallback: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        fallback.to_string()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(status: Option<i32>, stdout: &str, stderr: &str) -> HookRun {
        HookRun {
            status,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            timed_out: false,
        }
    }

    #[test]
    fn pre_reads_both_json_shapes_and_exit_two() {
        assert_eq!(
            pre_verdict(&run(Some(2), "", "no\n")),
            PreVerdict::Deny("no".into())
        );
        let deny = r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"r"}}"#;
        assert_eq!(pre_verdict(&run(Some(0), deny, "")), PreVerdict::Deny("r".into()));
        let ask = r#"{"hookSpecificOutput":{"permissionDecision":"ask","permissionDecisionReason":"sure?"}}"#;
        assert_eq!(pre_verdict(&run(Some(0), ask, "")), PreVerdict::Ask("sure?".into()));
        assert_eq!(
            pre_verdict(&run(Some(0), r#"{"decision":"block","reason":"legacy"}"#, "")),
            PreVerdict::Deny("legacy".into())
        );
        // allow, plain output, a failing hook: all decide nothing.
        let allow = r#"{"hookSpecificOutput":{"permissionDecision":"allow"}}"#;
        assert_eq!(pre_verdict(&run(Some(0), allow, "")), PreVerdict::Nothing);
        assert_eq!(pre_verdict(&run(Some(0), "just words", "")), PreVerdict::Nothing);
        assert_eq!(pre_verdict(&run(Some(1), "", "boom")), PreVerdict::Nothing);
        assert_eq!(
            pre_verdict(&HookRun {
                timed_out: true,
                ..run(None, "", "")
            }),
            PreVerdict::Nothing
        );
    }

    #[test]
    fn post_blocks_and_attaches_context() {
        let out = post_out(&run(Some(2), "", "needs review"));
        assert_eq!(out.block.as_deref(), Some("needs review"));
        let both = r#"{"decision":"block","reason":"redo","hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"files changed"}}"#;
        let out = post_out(&run(Some(0), both, ""));
        assert_eq!(out.block.as_deref(), Some("redo"));
        assert_eq!(out.context.as_deref(), Some("files changed"));
        let ctx_only = r#"{"hookSpecificOutput":{"additionalContext":"fyi"}}"#;
        let out = post_out(&run(Some(0), ctx_only, ""));
        assert_eq!(out.block, None);
        assert_eq!(out.context.as_deref(), Some("fyi"));
        assert_eq!(post_out(&run(Some(1), "", "err")).block, None);
    }
}
