//! The provider layer, which now lives in the `bough-llm` crate. This module
//! is the seam that keeps every `crate::llm::…` path working, plus the few
//! pieces that belong to bough rather than to the provider layer:
//!
//! - [`retry::is_retryable`] over `BoughError` (the host's error type);
//! - [`sse::blocks_to_parts`] — a finished round's blocks as persisted
//!   [`Part`](crate::schema::parts::Part)s;
//! - [`trace::TurnManifest`] / [`trace::write_manifest`] — the prompt-section
//!   manifest written beside a trace, because section identity is only
//!   knowable where assembly happened.
//!
//! The invariant is unchanged: **the turn runner must not know which provider
//! it is talking to.** Provider-specific handling must not leak past
//! `types::LlmClient`, and `bough_llm::LlmError` becomes `BoughError::Llm` at
//! this boundary (`impl From<LlmError> for BoughError`, `errors.rs`).

pub use bough_llm::{
    anthropic, client_for, complete_text, discovery, openai, openai_compat, pricing,
    provider_client, provider_for, routing, ClientOpts, CompleteTextOpts, Provider, ProviderOpts,
    RetryOpts, TraceLabel,
};

pub mod retry {
    pub use bough_llm::retry::*;

    use crate::errors::BoughError;

    /// Should this failure be re-attempted? The provider layer's table over
    /// its own error (`bough_llm::retry::is_retryable`), applied to the
    /// host's: an `LlmError` by its full rule (status plus the one
    /// self-healed tool-protocol 400), anything else by status alone.
    pub fn is_retryable(err: &BoughError) -> bool {
        match err.as_llm() {
            Some(llm) => bough_llm::retry::is_retryable(&llm),
            None => retryable_status(err.status()),
        }
    }
}

pub mod sse {
    pub use bough_llm::sse::*;

    use crate::schema::parts::Part;
    use crate::types::LlmBlock;

    /// A finished round's blocks → the parts persisted on the supervisor message.
    ///
    /// `model` stamps the reasoning parts, because a provider signature is only
    /// valid for the model that produced it and replay is gated on that. A
    /// reasoning block with NO displayable text is still persisted when it
    /// carries `meta` — that is a redacted thinking block, and the provider's
    /// rule is that a block comes back exactly as it was received or not at all.
    pub fn blocks_to_parts(blocks: &[LlmBlock], model: Option<&str>) -> Vec<Part> {
        let mut parts = Vec::new();
        for b in blocks {
            match b {
                LlmBlock::Text { text } => {
                    if !text.is_empty() {
                        parts.push(Part::Text { text: text.clone() });
                    }
                }
                LlmBlock::Reasoning { text, meta } => {
                    if !text.trim().is_empty() || meta.is_some() {
                        parts.push(Part::Reasoning {
                            text: text.clone(),
                            meta: meta.clone(),
                            model: model.map(String::from),
                        });
                    }
                }
                LlmBlock::ToolUse { id, name, input } => {
                    parts.push(Part::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    });
                }
            }
        }
        parts
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[test]
        fn blocks_to_parts_text_reasoning_and_tool_calls_map_across_in_order() {
            let blocks = vec![
                LlmBlock::Reasoning {
                    text: "weighing options".into(),
                    meta: Some(json!({ "signature": "sig" })),
                },
                LlmBlock::Text {
                    text: "here goes".into(),
                },
                LlmBlock::ToolUse {
                    id: "t1".into(),
                    name: "run_steps".into(),
                    input: json!({ "code": "1" }),
                },
            ];
            assert_eq!(
                blocks_to_parts(&blocks, Some("some-model")),
                vec![
                    Part::Reasoning {
                        text: "weighing options".into(),
                        meta: Some(json!({ "signature": "sig" })),
                        model: Some("some-model".into()),
                    },
                    Part::Text {
                        text: "here goes".into()
                    },
                    Part::ToolCall {
                        id: "t1".into(),
                        name: "run_steps".into(),
                        input: json!({ "code": "1" }),
                    },
                ]
            );
        }

        #[test]
        fn blocks_to_parts_the_signature_is_persisted_stamped_with_its_model() {
            // It is what lets the next turn replay the block verbatim. Providers
            // reject a thinking block whose content was altered, so the payload
            // has to survive the round trip through the database intact.
            let parts = blocks_to_parts(
                &[LlmBlock::Reasoning {
                    text: "hmm".into(),
                    meta: Some(json!({ "type": "thinking", "signature": "secret" })),
                }],
                Some("claude-opus-5"),
            );
            assert_eq!(
                parts,
                vec![Part::Reasoning {
                    text: "hmm".into(),
                    meta: Some(json!({ "type": "thinking", "signature": "secret" })),
                    model: Some("claude-opus-5".into()),
                }]
            );
        }

        #[test]
        fn blocks_to_parts_with_no_model_to_stamp_reasoning_stays_display_only() {
            // An unstamped part can never satisfy replay's model gate, which is
            // the conservative answer for a caller not building a live request.
            let parts = blocks_to_parts(
                &[LlmBlock::Reasoning {
                    text: "hmm".into(),
                    meta: Some(json!({ "type": "thinking", "signature": "s" })),
                }],
                None,
            );
            assert_eq!(
                parts,
                vec![Part::Reasoning {
                    text: "hmm".into(),
                    meta: Some(json!({ "type": "thinking", "signature": "s" })),
                    model: None,
                }]
            );
        }

        #[test]
        fn blocks_to_parts_a_redacted_block_persists_unsigned_empty_reasoning_does_not() {
            // A redacted thinking block has nothing displayable but must still go
            // back whole, so it is kept for its payload alone. Reasoning with
            // neither text nor payload is worth nothing to anyone.
            let parts = blocks_to_parts(
                &[
                    LlmBlock::Reasoning {
                        text: "".into(),
                        meta: Some(json!({ "type": "redacted_thinking" })),
                    },
                    LlmBlock::Reasoning {
                        text: "   \n ".into(),
                        meta: None,
                    },
                    LlmBlock::Text { text: "".into() },
                ],
                Some("m1"),
            );
            assert_eq!(
                parts,
                vec![Part::Reasoning {
                    text: "".into(),
                    meta: Some(json!({ "type": "redacted_thinking" })),
                    model: Some("m1".into()),
                }]
            );
        }

        #[test]
        fn blocks_to_parts_a_tool_call_with_no_input_still_yields_a_part() {
            // `stop` takes no arguments; the call is the whole message.
            let parts = blocks_to_parts(
                &[LlmBlock::ToolUse {
                    id: "t9".into(),
                    name: "stop".into(),
                    input: json!({}),
                }],
                None,
            );
            assert_eq!(
                parts,
                vec![Part::ToolCall {
                    id: "t9".into(),
                    name: "stop".into(),
                    input: json!({})
                }]
            );
        }
    }
}

pub mod trace {
    pub use bough_llm::trace::*;

    use std::fs::create_dir_all;

    use serde::{Deserialize, Serialize};

    use crate::prompt::assemble::SectionSha;

    /// What the turn knew that the provider boundary does not: which prompt
    /// sections went in, and what the turn was configured with.
    ///
    /// `LlmParams` carries the assembled prefix as one opaque string, so section
    /// identity has to be written from where assembly happened. That is the whole
    /// point of the manifest — an editable component is a SECTION, and without
    /// this the trace can only say "the prefix changed", never which file did.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub struct TurnManifest {
        pub session_id: String,
        pub turn_id: String,
        pub model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace: Option<String>,
        /// Every included section, in prompt order, with the sha of its text.
        pub sections: Vec<SectionSha>,
        pub started_at: i64,
    }

    /// Write a turn's manifest, pretty-printed. Called once, from where the
    /// prompt was assembled. As above: diagnostic, never fatal.
    pub fn write_manifest(label: &TraceLabel, manifest: &TurnManifest) {
        let path = manifest_path(label);
        let Some(parent) = path.parent() else { return };
        if create_dir_all(parent).is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(manifest) else {
            return;
        };
        let _ = std::fs::write(&path, format!("{text}\n"));
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::prompt::assemble::{section_sha, SectionId};

        fn label() -> TraceLabel {
            let dir = std::env::temp_dir().join(format!("bough-trace-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("mkdtemp");
            TraceLabel {
                dir: dir.to_string_lossy().into_owned(),
                session_id: "s1".into(),
                turn_id: "t1".into(),
            }
        }

        #[tokio::test]
        async fn the_manifest_carries_the_section_identities_the_raw_trace_cannot_see() {
            let l = label();
            let manifest = TurnManifest {
                session_id: "s1".into(),
                turn_id: "t1".into(),
                model: "claude-opus-5".into(),
                effort: Some("high".into()),
                workspace: Some("/w".into()),
                sections: vec![SectionSha {
                    id: SectionId::Identity,
                    sha: section_sha("x"),
                    bytes: 1,
                }],
                started_at: 1234,
            };
            write_manifest(&l, &manifest);
            let text = std::fs::read_to_string(manifest_path(&l)).unwrap();
            let parsed: TurnManifest = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed, manifest);
            assert!(text.ends_with('\n'));
            assert!(text.contains("\n  "), "pretty-printed for a human reader");
        }

        #[tokio::test]
        async fn an_unwritable_directory_never_kills_the_manifest() {
            let blocker =
                std::env::temp_dir().join(format!("bough-trace-blk-{}", uuid::Uuid::new_v4()));
            std::fs::write(&blocker, "not a directory").unwrap();
            let l = TraceLabel {
                dir: blocker.to_string_lossy().into_owned(),
                session_id: "s1".into(),
                turn_id: "t1".into(),
            };
            write_manifest(
                &l,
                &TurnManifest {
                    session_id: "s1".into(),
                    turn_id: "t1".into(),
                    model: "m".into(),
                    effort: None,
                    workspace: None,
                    sections: vec![],
                    started_at: 0,
                },
            );
            assert!(!manifest_path(&l).exists());
            let _ = std::fs::remove_file(&blocker);
        }
    }
}
