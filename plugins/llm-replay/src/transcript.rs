//! Invariant: replay is DETERMINISTIC. The same transcript answers the same request the same way
//! on every run and in every process, and an unmatched request fails loudly in strict mode rather
//! than yielding a silent empty answer.

use bough_plugin_llm::{Chunk, LlmRequest};

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
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    End {
        stop: String,
    },
    Failed {
        kind: String,
        message: String,
    },
}

impl RecordedChunk {
    /// WP-1.
    pub fn to_chunk(&self) -> Chunk {
        todo!("WP-1: total mapping from the recorded spelling onto the seam's Chunk")
    }
}

/// A whole transcript: rounds in the order they answer.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub rounds: Vec<Round>,
}

impl Transcript {
    /// Parse YAML or JSON. WP-1.
    pub fn parse(_text: &str) -> Result<Transcript, String> {
        todo!("WP-1: parse a transcript")
    }

    /// Pick the round that answers `req`, consuming it. Pure over an explicit cursor so the
    /// choice is testable without a runtime. WP-1.
    pub fn select(&self, _cursor: usize, _req: &LlmRequest) -> Option<(usize, &Round)> {
        todo!("WP-1: first unconsumed round whose `match` is a substring of the last user message")
    }
}
