//! Invariant (§12): a model failure is a TERMINAL CHUNK, never a thrown error, and a stream
//! carries exactly one terminal chunk with nothing after it. Every consumer therefore has one
//! failure shape to handle and the loop never branches on two.

use std::sync::Arc;

use bough_kernel::WaterfallEvent;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::ids::{AdapterName, ToolCallId, ToolName};
use crate::request::LlmRequest;

pub use bough_llm::types::Usage;

/// A stream of [`Chunk`]s, produced by the resolved adapter.
pub type LlmStream = futures::stream::BoxStream<'static, Chunk>;

/// One increment of a model round.
#[derive(Clone, Debug, PartialEq)]
pub enum Chunk {
    /// Assistant text as it arrives. The loop appends it as `thought/text`, coalesced per flush.
    TextDelta { text: String },
    /// Reasoning text. `meta` is an opaque provider payload, replayed verbatim.
    ReasoningDelta {
        text: String,
        meta: Option<serde_json::Value>,
    },
    /// A complete tool call.
    ToolCall {
        id: ToolCallId,
        name: ToolName,
        input: serde_json::Value,
    },
    /// Provider usage for the round.
    Usage(Usage),
    /// Terminal: the round finished.
    End { stop: StopReason },
    /// Terminal: the round failed.
    Failed(LlmFailure),
}

impl Chunk {
    /// `true` for [`Chunk::End`] and [`Chunk::Failed`], the two chunks that end a stream.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Chunk::End { .. } | Chunk::Failed(_))
    }
}

/// Why the model stopped.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

/// A terminal failure of one model round.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LlmFailure {
    pub kind: FailureKind,
    pub message: String,
    /// Whether `llm-retry` may retry it; the listener still decides.
    pub retryable: bool,
    pub status: Option<u16>,
    pub adapter: AdapterName,
}

/// The failure taxonomy `llm-retry`'s `retry_on` list is written in.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    Transport,
    RateLimit,
    Overloaded,
    ContextOverflow,
    Auth,
    BadRequest,
    Cancelled,
    Truncated,
    Other,
}

/// The one mutable cell of [`StreamCall`].
///
/// Deviation from the plan's `stream: Option<LlmStream>`, forced by the kernel:
/// `WaterfallEvent::Value` must be `Clone` and a `BoxStream` is not. The slot clones by sharing,
/// which is what a waterfall hop needs anyway — the value it hands on is the value it received.
#[derive(Clone, Default)]
pub struct StreamSlot(Arc<Mutex<Option<LlmStream>>>);

impl StreamSlot {
    /// An unfilled slot: what the executor hands to the first hop.
    pub fn empty() -> StreamSlot {
        StreamSlot::default()
    }
    /// Put a stream in the slot, replacing whatever was there.
    pub fn put(&self, s: LlmStream) {
        *self.0.lock() = Some(s);
    }
    /// Take the stream out, leaving the slot empty.
    pub fn take(&self) -> Option<LlmStream> {
        self.0.lock().take()
    }
    /// Whether some hop has filled it.
    pub fn is_filled(&self) -> bool {
        self.0.lock().is_some()
    }
}

impl std::fmt::Debug for StreamSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StreamSlot(filled: {})", self.is_filled())
    }
}

/// The value of the `llm/stream` waterfall.
#[derive(Clone, Debug)]
pub struct StreamCall {
    pub request: Arc<LlmRequest>,
    pub cancel: CancellationToken,
    /// Empty until the innermost hop fills it. A wrapper that returns without calling `next` and
    /// without filling it yields a `Chunk::Failed`, never a hang.
    pub stream: StreamSlot,
}

/// §5: a listener may observe or replace the stream; the innermost hop is the resolved adapter.
pub struct LlmStreamEvent;

impl WaterfallEvent for LlmStreamEvent {
    const NAME: &'static str = "llm/stream";
    type Value = StreamCall;
}
