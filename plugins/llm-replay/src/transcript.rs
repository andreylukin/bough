//! Invariant: replay is DETERMINISTIC. The same transcript answers the same request the same way
//! on every run and in every process, and an unmatched request fails loudly in strict mode rather
//! than yielding a silent empty answer.

use bough_plugin_llm::{
    AdapterName, Chunk, FailureKind, LlmContentBlock, LlmFailure, LlmRequest, LlmRole, StopReason,
    ToolCallId, ToolName,
};

/// One recorded round.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Round {
    /// A substring the last user message must contain for this round to match. `None` matches
    /// anything not already consumed.
    #[serde(default)]
    pub r#match: Option<String>,
    /// What the stream yields, in order. The last one must be terminal.
    pub chunks: Vec<RecordedChunk>,
}

/// The serialisable spelling of a [`Chunk`]. The seam's `Chunk` is not `Serialize` (it carries a
/// provider-opaque `meta`), so the transcript has its own shape and one total mapping.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecordedChunk {
    Text {
        text: String,
        /// Milliseconds to wait BEFORE yielding this chunk (P3-D20). `0` — the default, and what
        /// every existing transcript says — is exactly the Phase-2 behaviour.
        #[serde(default)]
        delay_ms: u64,
    },
    Reasoning {
        text: String,
        #[serde(default)]
        delay_ms: u64,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(default)]
        delay_ms: u64,
    },
    End {
        stop: String,
        #[serde(default)]
        delay_ms: u64,
    },
    Failed {
        kind: String,
        message: String,
        #[serde(default)]
        delay_ms: u64,
    },
}

impl RecordedChunk {
    /// TOTAL: every recorded spelling maps, and an unknown `stop` or `kind` word maps to a named
    /// default rather than a panic — a transcript is data, and bad data must not take the process.
    pub fn to_chunk(&self) -> Chunk {
        match self {
            RecordedChunk::Text { text, .. } => Chunk::TextDelta { text: text.clone() },
            RecordedChunk::Reasoning { text, .. } => Chunk::ReasoningDelta {
                text: text.clone(),
                meta: None,
            },
            RecordedChunk::ToolCall {
                id, name, input, ..
            } => Chunk::ToolCall {
                id: ToolCallId::new(id),
                name: ToolName::new(name),
                input: input.clone(),
            },
            RecordedChunk::End { stop, .. } => Chunk::End {
                stop: match stop.as_str() {
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    "stop_sequence" => StopReason::StopSequence,
                    _ => StopReason::EndTurn,
                },
            },
            RecordedChunk::Failed { kind, message, .. } => Chunk::Failed(LlmFailure {
                kind: parse_kind(kind),
                message: message.clone(),
                retryable: matches!(
                    parse_kind(kind),
                    FailureKind::Transport | FailureKind::RateLimit | FailureKind::Overloaded
                ),
                status: None,
                adapter: AdapterName::new(crate::PLUGIN_NAME),
            }),
        }
    }
}

impl RecordedChunk {
    /// How long to wait before this chunk is yielded. `0` unless the transcript says otherwise.
    pub fn delay_ms(&self) -> u64 {
        match self {
            RecordedChunk::Text { delay_ms, .. }
            | RecordedChunk::Reasoning { delay_ms, .. }
            | RecordedChunk::ToolCall { delay_ms, .. }
            | RecordedChunk::End { delay_ms, .. }
            | RecordedChunk::Failed { delay_ms, .. } => *delay_ms,
        }
    }
}

/// The recorded spelling of a [`FailureKind`]. Unknown words are `other`, never a panic.
pub fn parse_kind(s: &str) -> FailureKind {
    match s {
        "transport" => FailureKind::Transport,
        "rate_limit" => FailureKind::RateLimit,
        "overloaded" => FailureKind::Overloaded,
        "context_overflow" => FailureKind::ContextOverflow,
        "auth" => FailureKind::Auth,
        "bad_request" => FailureKind::BadRequest,
        "cancelled" => FailureKind::Cancelled,
        "truncated" => FailureKind::Truncated,
        _ => FailureKind::Other,
    }
}

/// The last user message of a request, flattened to text. The one thing a round's `match` is
/// tested against, so replay is a pure function of (transcript, request).
pub fn last_user_text(req: &LlmRequest) -> String {
    let Some(m) = req.messages.iter().rev().find(|m| m.role == LlmRole::User) else {
        return String::new();
    };
    m.content
        .iter()
        .map(|b| match b {
            LlmContentBlock::Text { text } => text.clone(),
            LlmContentBlock::ToolResult { content, .. } => content.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A whole transcript: rounds in the order they answer.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub rounds: Vec<Round>,
}

impl Transcript {
    /// Parse YAML (JSON is a subset of YAML, so one parser covers both).
    ///
    /// A bare list of rounds is accepted as well as `{ rounds: [...] }`: a fixture file that is
    /// just a list is the common shape, and refusing it would be a papercut with no upside.
    pub fn parse(text: &str) -> Result<Transcript, String> {
        let value: serde_yaml::Value =
            serde_yaml::from_str(text).map_err(|e| format!("transcript is not valid YAML: {e}"))?;
        Transcript::from_value(value)
    }

    /// The same parse over an already-decoded value: the `rounds:` config field is inline JSON.
    pub fn from_value(value: serde_yaml::Value) -> Result<Transcript, String> {
        if value.is_sequence() {
            let rounds: Vec<Round> = serde_yaml::from_value(value)
                .map_err(|e| format!("transcript rounds do not parse: {e}"))?;
            return Ok(Transcript { rounds });
        }
        serde_yaml::from_value(value).map_err(|e| format!("transcript does not parse: {e}"))
    }

    /// Pick the round that answers `req`, given how many rounds are already consumed.
    ///
    /// Pure over an explicit cursor, so the choice is testable without a runtime and identical in
    /// every process: the first UNCONSUMED round whose `match` is a substring of the last user
    /// message, or whose `match` is absent.
    pub fn select(&self, cursor: usize, req: &LlmRequest) -> Option<(usize, &Round)> {
        let text = last_user_text(req);
        self.rounds
            .iter()
            .enumerate()
            .skip(cursor)
            .find(|(_, r)| match &r.r#match {
                None => true,
                Some(m) => text.contains(m.as_str()),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_llm::{CallConfig, LlmMessage};

    fn req(text: &str) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            system: None,
            system_volatile: None,
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::Text { text: text.into() }],
            }],
            tools: vec![],
            call: CallConfig {
                model: "m".into(),
                max_tokens: 8,
                effort: None,
                tool_choice_none: false,
                meta: Default::default(),
            },
        }
    }

    fn transcript() -> Transcript {
        Transcript::parse(
            r#"
rounds:
  - match: "hello"
    chunks:
      - { type: text, text: "hi" }
      - { type: end, stop: end_turn }
  - chunks:
      - { type: end, stop: end_turn }
"#,
        )
        .expect("parses")
    }

    #[test]
    fn a_bare_list_of_rounds_parses_too() {
        let t = Transcript::parse("- chunks: [{ type: end, stop: end_turn }]").expect("parses");
        assert_eq!(t.rounds.len(), 1);
    }

    #[test]
    fn selection_is_the_first_unconsumed_matching_round() {
        let t = transcript();
        assert_eq!(
            t.select(0, &req("say hello please")).map(|(i, _)| i),
            Some(0)
        );
        // Consumed: the next unconsumed round has no `match` and answers anything.
        assert_eq!(
            t.select(1, &req("say hello please")).map(|(i, _)| i),
            Some(1)
        );
        assert_eq!(t.select(2, &req("say hello please")), None);
        // A round whose `match` misses is skipped, not forced.
        assert_eq!(t.select(0, &req("goodbye")).map(|(i, _)| i), Some(1));
    }

    #[test]
    fn a_chunk_delay_defaults_to_zero_and_is_read_when_given() {
        let t = Transcript::parse(
            "- chunks: [{ type: text, text: hi, delay_ms: 25 }, { type: end, stop: end_turn }]",
        )
        .expect("parses");
        assert_eq!(t.rounds[0].chunks[0].delay_ms(), 25);
        assert_eq!(t.rounds[0].chunks[1].delay_ms(), 0, "the default is 0");
    }

    #[test]
    fn every_recorded_spelling_maps_to_a_chunk() {
        assert!(matches!(
            RecordedChunk::End {
                stop: "tool_use".into(),
                delay_ms: 0
            }
            .to_chunk(),
            Chunk::End {
                stop: StopReason::ToolUse
            }
        ));
        // An unknown word does not panic.
        assert!(matches!(
            RecordedChunk::End {
                stop: "who knows".into(),
                delay_ms: 0
            }
            .to_chunk(),
            Chunk::End {
                stop: StopReason::EndTurn
            }
        ));
        assert_eq!(parse_kind("nonsense"), FailureKind::Other);
        assert_eq!(parse_kind("rate_limit"), FailureKind::RateLimit);
    }
}
