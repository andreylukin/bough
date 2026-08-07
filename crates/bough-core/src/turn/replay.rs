//! Stored parts → provider messages (port of `src/turn/replay.ts`). The one
//! place a persisted transcript becomes something an `LlmClient` can be
//! handed.
//!
//! WHY IT IS A SEPARATE MODULE. The mapping is pure and total, and it is where
//! two invariants live that nothing else in the system can enforce:
//!
//! **1. Reasoning replays only to the model that signed it.** A `reasoning`
//! part is persisted so the UI can fold it open, and — when the provider gave
//! it a `meta` payload — so it can be sent back. The rule providers state is
//! that a thinking block returns EXACTLY as received or not at all: they
//! reject a block whose content was modified, not one that was merely read.
//! So `meta` goes back untouched, and the *text* is never reconstructed into a
//! block on its own — an unsigned imitation of thinking is both wrong and
//! billable.
//!
//! The gate is the model, and nothing else. A signature is scoped to the model
//! that produced it, which is true of every provider, so this needs no
//! knowledge of which one is in play: [`message_to_llm`] compares
//! `part.model` to the model being asked and hands the block through untouched
//! when they match. What that payload is worth is then the provider mapper's
//! business, which is the only place that ever looks inside it.
//!
//! Dropping reasoning is NOT the conservative default it looks like — removing
//! thinking blocks can itself provoke ordering and signature errors, and a
//! mismatched model discards them server-side without billing. This module
//! therefore drops only what it cannot vouch for: a part with no `meta`, or
//! one signed by a different model. The *in-turn* echo is a separate mechanism
//! in `runner.rs`, which never consults the database at all.
//!
//! **2. `ask` parts replay as plain text and can never re-block.** A settled
//! hold is a fact about what the user said, not a live question. Replaying it
//! as anything the harness could re-raise would park a rebuilt thread on a
//! question the user answered days ago, with no UI attached to answer it
//! again. It becomes `[ask] <question> → the user answered: <answer>` in the
//! user-side message, after the tool results — a tool_use's result must lead
//! the user message that follows it, and text jammed in front of it is a
//! provider 400.
//!
//! Two smaller rules that are equally not rediscoverable:
//!
//! - **A `tool_use` with no matching `tool_result` gets a synthetic one.** A
//!   crash, an orphaned turn, or an interrupt between the call and its result
//!   leaves the pair open, and every provider rejects a thread in that state.
//!   The synthetic result says `(interrupted)` rather than pretending the tool
//!   succeeded.
//! - **A lost attachment replays as placeholder text, never as a failure.**
//!   The bytes live outside the parts JSON precisely so a row survives the
//!   file moving; the replay has to survive it too, or one deleted screenshot
//!   makes an entire session unreplayable.
//!
//! Purity: the image loader is injected. [`message_to_llm`] reads nothing and
//! calls no clock, so the whole mapping is testable with no filesystem and no
//! `~/.bough`.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::Value;

use crate::paths::attachments_dir;
use crate::schema::parts::{AskStatus, Message, Part, Role};
use crate::types::{LlmContentBlock, LlmMessage, LlmRole};

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

/// The fields of one [`Part::Image`], borrowed — replay hands these to the
/// loader instead of the whole enum so a test can build one in a line.
#[derive(Clone, Copy, Debug)]
pub struct ImageRef<'a> {
    pub path: &'a str,
    pub media_type: &'a str,
    pub name: &'a str,
    pub size: i64,
}

/// What a loader answers with when the attachment is still there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedImage {
    /// base64.
    pub data: String,
    pub media_type: String,
}

/// Loads an image part's bytes for replay. Returns `None` when the attachment
/// is gone — the caller degrades to placeholder text rather than failing the
/// turn. Injected so replay is pure: the tests pass a closure, production
/// passes [`read_attachment`].
pub type ImageLoader<'a> = &'a dyn Fn(&ImageRef<'_>) -> Option<LoadedImage>;

/// Resolve an image part's stored path. Relative paths resolve under
/// `~/.bough/attachments`, which is where every attachment is written; an
/// absolute path is taken as written, because it is one this server stored in
/// its own database, not a name that arrived in a request. (Contrast
/// `confine`, which guards path construction from *request* input — a
/// different job.)
pub fn attachment_path(part: &ImageRef<'_>) -> PathBuf {
    let p = Path::new(part.path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        attachments_dir().join(part.path)
    }
}

/// The production loader: read the file, base64 it. Every failure mode —
/// missing, unreadable, no permission — is the same answer, `None`, because
/// the replay's response to all of them is identical and a distinction the
/// caller cannot act on is noise.
pub fn read_attachment(part: &ImageRef<'_>) -> Option<LoadedImage> {
    let bytes = std::fs::read(attachment_path(part)).ok()?;
    Some(LoadedImage {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        media_type: part.media_type.to_string(),
    })
}

/// What the model sees in place of an image whose bytes are gone.
///
/// It names the file the user named, and says plainly that the bytes are the
/// missing thing — not that the user sent nothing. A model told "[image]" with
/// no qualification will describe a picture it cannot see; told this, it asks
/// for it again or works without it (error text is a product surface).
pub fn lost_attachment_text(part: &ImageRef<'_>) -> String {
    format!(
        "[image: {} — the attachment is no longer on disk, so it cannot be shown \
         this time. It was {} bytes. Ask for it again if you need to see it.]",
        part.name, part.size
    )
}

// ---------------------------------------------------------------------------
// One message
// ---------------------------------------------------------------------------

/// Options for [`message_to_llm`].
#[derive(Clone, Copy, Default)]
pub struct ReplayOptions<'a> {
    /// Defaults to [`read_attachment`].
    pub load_image: Option<ImageLoader<'a>>,
    /// The model this thread is being replayed FOR. Reasoning replays only to
    /// the model that signed it (invariant 1); `None` means no reasoning
    /// replays at all, which is the right answer for any caller rebuilding a
    /// thread for something other than a live request — a UI, an export, a
    /// test.
    pub model: Option<&'a str>,
}

/// Tool output is persisted as JSON; the wire wants a string.
///
/// A string passes through; the absent-output default (`null`, the TS
/// `undefined`) becomes `""`; everything else is its JSON text. Serializing a
/// `Value` cannot cycle, but the TS contract stands: this must never fail.
pub fn stringify_output(output: &Value) -> String {
    match output {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// How a settled hold reads to the model. Past tense, always — invariant 2.
fn ask_text(question: &str, status: AskStatus, answer: Option<&str>) -> String {
    let outcome = match status {
        AskStatus::Answered => format!("the user answered: {}", answer.unwrap_or("")),
        AskStatus::Declined => "the user declined to answer".to_string(),
        AskStatus::Interrupted => "the turn was interrupted before an answer".to_string(),
    };
    format!("[ask] {question}\n→ {outcome}")
}

/// One stored message → zero, one or two provider messages.
///
/// - `user` and `system` → **one user message** of text and image blocks.
///   System notes (a detached subagent's report, a background job's exit,
///   artifact comments) are input *to* the model, never words it said, so they
///   replay user-side.
/// - `supervisor` → an **assistant message** (text + tool_use), then, when the
///   round produced results or settled a hold, a **user message** of
///   tool_result blocks followed by ask text.
///
/// Empty in, empty out: a message that maps to no blocks yields no message at
/// all rather than an empty one, which providers reject.
pub fn message_to_llm(m: &Message, opts: &ReplayOptions<'_>) -> Vec<LlmMessage> {
    if matches!(m.role, Role::User | Role::System) {
        let mut content: Vec<LlmContentBlock> = Vec::new();
        for p in &m.parts {
            match p {
                Part::Text { text } => {
                    if !text.is_empty() {
                        content.push(LlmContentBlock::Text { text: text.clone() });
                    }
                }
                Part::Image {
                    path,
                    media_type,
                    name,
                    size,
                } => {
                    let r = ImageRef {
                        path,
                        media_type,
                        name,
                        size: *size,
                    };
                    let loaded = match opts.load_image {
                        Some(f) => f(&r),
                        None => read_attachment(&r),
                    };
                    content.push(match loaded {
                        Some(img) => LlmContentBlock::Image {
                            data: img.data,
                            media_type: img.media_type,
                            name: name.clone(),
                        },
                        None => LlmContentBlock::Text {
                            text: lost_attachment_text(&r),
                        },
                    });
                }
                // Every other part kind is supervisor-side and cannot appear here.
                _ => {}
            }
        }
        if content.is_empty() {
            return vec![];
        }
        return vec![LlmMessage {
            role: LlmRole::User,
            content,
        }];
    }

    let mut assistant: Vec<LlmContentBlock> = Vec::new();
    let mut results: Vec<LlmContentBlock> = Vec::new();
    let mut asks: Vec<LlmContentBlock> = Vec::new();
    let mut requested: Vec<String> = Vec::new();
    let mut resolved: std::collections::HashSet<String> = std::collections::HashSet::new();

    for p in &m.parts {
        match p {
            Part::Text { text } => {
                if !text.is_empty() {
                    assistant.push(LlmContentBlock::Text { text: text.clone() });
                }
            }
            Part::Reasoning { text, meta, model } => {
                // Invariant 1. A signed block replays verbatim to the model
                // that signed it; anything else is display-only and emits
                // nothing.
                if meta.is_some() && opts.model.is_some() && model.as_deref() == opts.model {
                    assistant.push(LlmContentBlock::Reasoning {
                        text: text.clone(),
                        meta: meta.clone(),
                    });
                }
            }
            Part::ToolCall { id, name, input } => {
                requested.push(id.clone());
                assistant.push(LlmContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }
            Part::ToolResult {
                call_id,
                output,
                is_error,
                ..
            } => {
                resolved.insert(call_id.clone());
                results.push(LlmContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: stringify_output(output),
                    is_error: *is_error,
                });
            }
            Part::Ask {
                question,
                status,
                answer,
                ..
            } => {
                asks.push(LlmContentBlock::Text {
                    text: ask_text(question, *status, answer.as_deref()),
                });
            }
            // A picture the supervisor produced reaches the model as a system
            // note carrying the part, never inline on its own message.
            Part::Image { .. } => {}
            // Display only. The run is detached: its outcome reaches the model
            // as the `[workflow done]` system note, which is the record replay
            // must not duplicate — echoing a launch line here would have the
            // model reading "started" and "finished" as two separate runs.
            Part::Workflow { .. } => {}
        }
    }

    // Close every open pair, in call order, so the thread is one a provider
    // accepts.
    for id in requested {
        if !resolved.contains(&id) {
            results.push(LlmContentBlock::ToolResult {
                tool_use_id: id,
                content: "(interrupted — this call never returned a result)".to_string(),
                is_error: true,
            });
        }
    }

    let mut out: Vec<LlmMessage> = Vec::new();
    if !assistant.is_empty() {
        out.push(LlmMessage {
            role: LlmRole::Assistant,
            content: assistant,
        });
    }
    // Results lead; ask text follows. Reversing this is a provider 400.
    if !results.is_empty() || !asks.is_empty() {
        let mut content = results;
        content.extend(asks);
        out.push(LlmMessage {
            role: LlmRole::User,
            content,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// A whole thread
// ---------------------------------------------------------------------------

/// Options for [`build_thread`].
#[derive(Clone, Copy, Default)]
pub struct ThreadOptions<'a> {
    pub load_image: Option<ImageLoader<'a>>,
    pub model: Option<&'a str>,
    /// A message id to leave out — the pending supervisor message the turn is
    /// currently producing. Replaying the thing you are about to write would
    /// show the model an empty assistant turn at the end of its own history.
    pub exclude: Option<&'a str>,
}

/// Root→leaf thread → provider messages.
///
/// Takes the already-ordered message list rather than a `Db`, because
/// ordering is the database's contract (`thread_for`: ancestors root→parent,
/// then own, each by `(created_at, rowid)`) and re-deriving it here would put
/// two answers in the tree.
pub fn build_thread(messages: &[Message], opts: &ThreadOptions<'_>) -> Vec<LlmMessage> {
    let replay = ReplayOptions {
        load_image: opts.load_image,
        model: opts.model,
    };
    let mut out: Vec<LlmMessage> = Vec::new();
    for m in messages {
        if opts.exclude.is_some_and(|ex| m.id == ex) {
            continue;
        }
        out.extend(message_to_llm(m, &replay));
    }
    out
}

/// Drop every reasoning block from an in-flight exchange.
///
/// The in-turn echo (runner.rs) is only valid while the model that produced
/// the thinking is still the one being asked. It is not valid across a model
/// swap, and it is not valid once a provider has rejected the round — a stale
/// or unverifiable signature is a hard 400, and the round's text and tool
/// calls are worth keeping even when its thinking is not. An assistant message
/// left with nothing but reasoning disappears with it, because a content-less
/// message is itself a 400.
pub fn strip_reasoning(messages: &mut Vec<LlmMessage>) {
    for i in (0..messages.len()).rev() {
        if messages[i].role != LlmRole::Assistant {
            continue;
        }
        messages[i]
            .content
            .retain(|b| !matches!(b, LlmContentBlock::Reasoning { .. }));
        if messages[i].content.is_empty() {
            messages.remove(i);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — port of `src/turn/replay.test.ts`. No filesystem, no database.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn message(role: Role, parts: Vec<Part>) -> Message {
        let n = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        Message {
            id: format!("m{n}"),
            session_id: "s1".to_string(),
            role,
            parts,
            pending: false,
            created_at: 1_000 + n as i64,
        }
    }

    fn image_part() -> Part {
        Part::Image {
            path: "abc.png".into(),
            media_type: "image/png".into(),
            name: "screenshot.png".into(),
            size: 4_096,
        }
    }

    fn image_ref() -> ImageRef<'static> {
        ImageRef {
            path: "abc.png",
            media_type: "image/png",
            name: "screenshot.png",
            size: 4_096,
        }
    }

    /// A loader that always answers, so the "found" path needs no file.
    fn found(_: &ImageRef<'_>) -> Option<LoadedImage> {
        Some(LoadedImage {
            data: "AAAA".into(),
            media_type: "image/png".into(),
        })
    }
    /// A loader that never answers — the moved/deleted attachment.
    fn lost(_: &ImageRef<'_>) -> Option<LoadedImage> {
        None
    }

    fn types(blocks: &[LlmContentBlock]) -> Vec<&'static str> {
        blocks
            .iter()
            .map(|b| match b {
                LlmContentBlock::Text { .. } => "text",
                LlmContentBlock::Reasoning { .. } => "reasoning",
                LlmContentBlock::ToolUse { .. } => "tool_use",
                LlmContentBlock::ToolResult { .. } => "tool_result",
                LlmContentBlock::Image { .. } => "image",
            })
            .collect()
    }

    fn text_of(b: &LlmContentBlock) -> &str {
        match b {
            LlmContentBlock::Text { text } => text,
            _ => panic!("not a text block"),
        }
    }

    // ---- user and system messages ------------------------------------------

    #[test]
    fn a_user_message_becomes_one_user_message_of_text_and_image_blocks() {
        let m = message(
            Role::User,
            vec![
                Part::Text {
                    text: "look at this".into(),
                },
                image_part(),
            ],
        );
        let out = message_to_llm(
            &m,
            &ReplayOptions {
                load_image: Some(&found),
                model: None,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, LlmRole::User);
        assert_eq!(types(&out[0].content), vec!["text", "image"]);
        assert_eq!(
            out[0].content[1],
            LlmContentBlock::Image {
                data: "AAAA".into(),
                media_type: "image/png".into(),
                name: "screenshot.png".into(),
            }
        );
    }

    #[test]
    fn a_lost_attachment_replays_as_placeholder_text_never_as_a_failure() {
        let m = message(Role::User, vec![image_part()]);
        let out = message_to_llm(
            &m,
            &ReplayOptions {
                load_image: Some(&lost),
                model: None,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(types(&out[0].content), vec!["text"]);
        let placeholder = text_of(&out[0].content[0]);
        assert_eq!(placeholder, lost_attachment_text(&image_ref()));
        // It names the file and says the BYTES are what is missing — a model
        // told only "[image]" describes a picture it cannot see.
        assert!(placeholder.contains("screenshot.png"));
        assert!(placeholder.contains("no longer on disk"));
    }

    #[test]
    fn a_system_note_replays_user_side() {
        // It is input to the model, not words it said.
        let m = message(
            Role::System,
            vec![Part::Text {
                text: "[subagent finished] audit-handlers".into(),
            }],
        );
        let out = message_to_llm(&m, &ReplayOptions::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, LlmRole::User);
    }

    #[test]
    fn a_message_with_nothing_to_say_produces_no_message_at_all() {
        let opts = ReplayOptions::default();
        assert!(message_to_llm(&message(Role::User, vec![]), &opts).is_empty());
        assert!(message_to_llm(
            &message(Role::User, vec![Part::Text { text: "".into() }]),
            &opts
        )
        .is_empty());
        assert!(message_to_llm(&message(Role::Supervisor, vec![]), &opts).is_empty());
    }

    // ---- supervisor messages -----------------------------------------------

    #[test]
    fn a_supervisor_round_becomes_an_assistant_message_and_then_its_tool_results() {
        let m = message(
            Role::Supervisor,
            vec![
                Part::Text {
                    text: "Running it.".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({"code":"1"}),
                },
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: json!("ok"),
                    is_error: false,
                    interrupted: None,
                },
            ],
        );
        let out = message_to_llm(&m, &ReplayOptions::default());

        assert_eq!(
            out.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![LlmRole::Assistant, LlmRole::User]
        );
        assert_eq!(types(&out[0].content), vec!["text", "tool_use"]);
        assert_eq!(
            out[1].content,
            vec![LlmContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "ok".into(),
                is_error: false,
            }]
        );
    }

    #[test]
    fn signed_reasoning_replays_verbatim_to_the_model_that_signed_it() {
        let meta = json!({"type":"thinking","thinking":"step one","signature":"sig-abc"});
        let m = message(
            Role::Supervisor,
            vec![
                Part::Reasoning {
                    text: "step one".into(),
                    meta: Some(meta.clone()),
                    model: Some("m1".into()),
                },
                Part::Text {
                    text: "Here is the answer.".into(),
                },
            ],
        );
        let out = message_to_llm(
            &m,
            &ReplayOptions {
                load_image: None,
                model: Some("m1"),
            },
        );
        assert_eq!(types(&out[0].content), vec!["reasoning", "text"]);
        match &out[0].content[0] {
            LlmContentBlock::Reasoning { meta: got, .. } => {
                assert_eq!(
                    got.as_ref(),
                    Some(&meta),
                    "the payload survives the db untouched"
                )
            }
            other => panic!("expected reasoning, got {other:?}"),
        }
    }

    #[test]
    fn signed_reasoning_does_not_replay_to_a_different_model() {
        // A signature is scoped to the model that produced it. This is the
        // whole gate: no provider is named anywhere, because the rule holds
        // for all of them.
        let m = message(
            Role::Supervisor,
            vec![
                Part::Reasoning {
                    text: "step one".into(),
                    meta: Some(json!({"signature":"s"})),
                    model: Some("m1".into()),
                },
                Part::Text {
                    text: "Here is the answer.".into(),
                },
            ],
        );
        let out = message_to_llm(
            &m,
            &ReplayOptions {
                load_image: None,
                model: Some("m2"),
            },
        );
        assert_eq!(types(&out[0].content), vec!["text"]);
    }

    #[test]
    fn unsigned_reasoning_never_replays_whatever_model_is_asking() {
        // Rows written before signatures were persisted, and providers that
        // give none. Re-sending the text alone would be an unsigned imitation
        // of thinking.
        for model in [None, Some("m1")] {
            let m = message(
                Role::Supervisor,
                vec![
                    Part::Reasoning {
                        text: "SECRET-THINKING".into(),
                        meta: None,
                        model: Some("m1".into()),
                    },
                    Part::Text {
                        text: "Here is the answer.".into(),
                    },
                ],
            );
            let out = message_to_llm(
                &m,
                &ReplayOptions {
                    load_image: None,
                    model,
                },
            );
            assert!(!serde_json::to_string(&out)
                .unwrap()
                .contains("SECRET-THINKING"));
        }
    }

    #[test]
    fn reasoning_is_dropped_on_replay_and_takes_nothing_else_with_it() {
        let m = message(
            Role::Supervisor,
            vec![
                Part::Reasoning {
                    text: "SECRET-THINKING".into(),
                    meta: None,
                    model: None,
                },
                Part::Text {
                    text: "Here is the answer.".into(),
                },
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({"code":"1"}),
                },
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: json!("ok"),
                    is_error: false,
                    interrupted: None,
                },
            ],
        );
        let out = message_to_llm(&m, &ReplayOptions::default());
        assert!(!serde_json::to_string(&out)
            .unwrap()
            .contains("SECRET-THINKING"));
        assert_eq!(
            types(&out[0].content),
            vec!["text", "tool_use"],
            "everything else survives"
        );
    }

    #[test]
    fn a_reasoning_only_message_vanishes_rather_than_replaying_as_an_empty_turn() {
        let m = message(
            Role::Supervisor,
            vec![Part::Reasoning {
                text: "hm".into(),
                meta: None,
                model: None,
            }],
        );
        assert!(message_to_llm(&m, &ReplayOptions::default()).is_empty());
    }

    #[test]
    fn a_settled_ask_replays_as_plain_text_after_the_tool_results() {
        let m = message(
            Role::Supervisor,
            vec![
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({"code":"1"}),
                },
                Part::Ask {
                    id: "q1".into(),
                    question: "Which branch?".into(),
                    options: Some(vec!["main".into(), "next".into()]),
                    status: AskStatus::Answered,
                    answer: Some("next".into()),
                },
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: json!("ok"),
                    is_error: false,
                    interrupted: None,
                },
            ],
        );
        let out = message_to_llm(&m, &ReplayOptions::default());

        // A tool_use's result must LEAD the user message that follows it; text
        // in front of it is a provider 400. The ask therefore lands after,
        // whatever the part order.
        assert_eq!(types(&out[1].content), vec!["tool_result", "text"]);
        let replayed = text_of(&out[1].content[1]);
        assert_eq!(replayed, "[ask] Which branch?\n→ the user answered: next");
        // Nothing carries the hold forward: the ask arrives as a `text` block
        // and there is no block type left the harness could re-raise from.
        assert!(!serde_json::to_string(&out).unwrap().contains("\"ask\""));
    }

    #[test]
    fn a_declined_or_interrupted_ask_says_which_in_the_past_tense() {
        let declined = message_to_llm(
            &message(
                Role::Supervisor,
                vec![Part::Ask {
                    id: "q1".into(),
                    question: "Proceed?".into(),
                    options: None,
                    status: AskStatus::Declined,
                    answer: None,
                }],
            ),
            &ReplayOptions::default(),
        );
        assert!(text_of(&declined[0].content[0]).contains("the user declined to answer"));

        let cut = message_to_llm(
            &message(
                Role::Supervisor,
                vec![Part::Ask {
                    id: "q2".into(),
                    question: "Proceed?".into(),
                    options: None,
                    status: AskStatus::Interrupted,
                    answer: None,
                }],
            ),
            &ReplayOptions::default(),
        );
        assert!(text_of(&cut[0].content[0]).contains("the turn was interrupted before an answer"));
    }

    #[test]
    fn a_tool_use_with_no_result_gets_a_synthetic_one_so_the_thread_stays_valid() {
        // The shape a crash, an orphaned turn or an interrupt between call and
        // result leaves behind. Every provider rejects the open pair.
        let m = message(
            Role::Supervisor,
            vec![
                Part::ToolCall {
                    id: "c1".into(),
                    name: "run_steps".into(),
                    input: json!({"code":"1"}),
                },
                Part::ToolCall {
                    id: "c2".into(),
                    name: "run_steps".into(),
                    input: json!({"code":"2"}),
                },
                Part::ToolResult {
                    call_id: "c1".into(),
                    output: json!("ok"),
                    is_error: false,
                    interrupted: None,
                },
            ],
        );
        let out = message_to_llm(&m, &ReplayOptions::default());

        let results: Vec<(&str, bool, &str)> = out[1]
            .content
            .iter()
            .map(|b| match b {
                LlmContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    content,
                } => (tool_use_id.as_str(), *is_error, content.as_str()),
                other => panic!("expected tool_result, got {other:?}"),
            })
            .collect();
        assert_eq!(
            results.iter().map(|r| r.0).collect::<Vec<_>>(),
            vec!["c1", "c2"],
            "in call order"
        );
        assert!(results[1].1);
        assert!(results[1].2.contains("interrupted"));
    }

    #[test]
    fn non_string_tool_output_is_stringified_rather_than_dropped() {
        assert_eq!(stringify_output(&json!("plain")), "plain");
        assert_eq!(stringify_output(&json!({"a":1})), r#"{"a":1}"#);
        // The absent-output default (TS `undefined`) is the empty string.
        assert_eq!(stringify_output(&Value::Null), "");
        assert_eq!(stringify_output(&json!([1, "two"])), r#"[1,"two"]"#);
    }

    // ---- whole threads -----------------------------------------------------

    #[test]
    fn a_thread_replays_in_order_minus_the_message_being_written() {
        let user = message(
            Role::User,
            vec![Part::Text {
                text: "do it".into(),
            }],
        );
        let supervisor = message(
            Role::Supervisor,
            vec![
                Part::Reasoning {
                    text: "thinking".into(),
                    meta: None,
                    model: None,
                },
                Part::Text {
                    text: "done".into(),
                },
            ],
        );
        let pending = message(Role::Supervisor, vec![]);
        let pending_id = pending.id.clone();

        let all = vec![user, supervisor, pending];
        let out = build_thread(
            &all,
            &ThreadOptions {
                exclude: Some(&pending_id),
                ..Default::default()
            },
        );
        assert_eq!(
            out.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![LlmRole::User, LlmRole::Assistant]
        );
        assert!(!serde_json::to_string(&out).unwrap().contains("thinking"));

        // Without the exclusion the pending message would still contribute
        // nothing — but the caller must not rely on that, since a
        // partially-written one would.
        assert_eq!(build_thread(&all, &ThreadOptions::default()).len(), 2);
    }

    #[test]
    fn strip_reasoning_drops_in_turn_thinking_and_the_messages_left_empty_by_it() {
        let mut messages = vec![
            LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::Text { text: "go".into() }],
            },
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![
                    LlmContentBlock::Reasoning {
                        text: "hm".into(),
                        meta: Some(json!({"signature":"sig"})),
                    },
                    LlmContentBlock::Text { text: "ok".into() },
                ],
            },
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![LlmContentBlock::Reasoning {
                    text: "only thinking".into(),
                    meta: None,
                }],
            },
        ];
        strip_reasoning(&mut messages);

        assert_eq!(messages.len(), 2, "the thinking-only message went with it");
        assert_eq!(types(&messages[1].content), vec!["text"]);
    }
}
