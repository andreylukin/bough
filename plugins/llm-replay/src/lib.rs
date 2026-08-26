//! Invariant: this crate is the OFFLINE `llm` provider. Everything the hermetic suite runs
//! against goes through it (AGENTS.md: the default suite never touches the network), and its
//! answers are a pure function of the transcript and the request.

pub mod invariant;
pub mod transcript;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_llm::{
    AdapterName, AdapterSpec, Chunk, FailureKind, Llm, LlmAdapter, LlmFailure, LlmRequest,
    LlmStream, ModelMatch,
};
use tokio_util::sync::CancellationToken;

pub use transcript::{RecordedChunk, Round, Transcript};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "llm-replay";

/// The row's config: a file, or the rounds inline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Rounds written straight into the patch, for a test that wants no fixture file. Raw JSON,
    /// parsed by [`Transcript::parse`] — the config schema stays shallow on purpose.
    #[serde(default)]
    pub rounds: Option<serde_json::Value>,
    /// `true` (the default): an unmatched request is `Chunk::Failed { BadRequest }`.
    #[serde(default = "yes")]
    pub strict: bool,
    /// Which models this adapter claims. `"*"` by default, so a swap patch needs one line.
    #[serde(default = "star")]
    pub models: String,
}

fn yes() -> bool {
    true
}
fn star() -> String {
    "*".to_string()
}

/// The replaying adapter.
///
/// The cursor is the ONLY mutable state: rounds are answered in order, and `select` is pure over
/// it, so two runs of the same transcript against the same requests answer identically.
pub struct ReplayAdapter {
    cfg: Arc<ReplayConfig>,
    transcript: Transcript,
    cursor: parking_lot::Mutex<usize>,
}

impl ReplayAdapter {
    /// An adapter over an already-parsed transcript.
    pub fn new(cfg: Arc<ReplayConfig>, transcript: Transcript) -> ReplayAdapter {
        ReplayAdapter {
            cfg,
            transcript,
            cursor: parking_lot::Mutex::new(0),
        }
    }

    /// Load the transcript this config names. The one place `transcript:` becomes rounds.
    pub fn load(cfg: &ReplayConfig) -> Result<Transcript, String> {
        match (&cfg.transcript, &cfg.rounds) {
            (Some(path), _) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read transcript `{}`: {e}", path.display()))?;
                Transcript::parse(&text)
            }
            (None, Some(rounds)) => {
                let value = serde_yaml::to_value(rounds)
                    .map_err(|e| format!("inline rounds do not re-encode: {e}"))?;
                Transcript::from_value(value)
            }
            (None, None) => Err("llm-replay needs either `transcript:` or `rounds:`".to_string()),
        }
    }

    /// The chunks this request is answered with, and whether a round was consumed.
    ///
    /// Pure but for the cursor, so the strict-mode refusal is testable without a runtime.
    pub fn answer(&self, req: &LlmRequest) -> Vec<Chunk> {
        let mut cursor = self.cursor.lock();
        match self.transcript.select(*cursor, req) {
            Some((index, round)) => {
                *cursor = index + 1;
                round.chunks.iter().map(RecordedChunk::to_chunk).collect()
            }
            None if self.cfg.strict => vec![Chunk::Failed(LlmFailure {
                kind: FailureKind::BadRequest,
                // Names the request, because "the transcript ran out" and "no round matches this
                // message" are different bugs and the message must say which.
                message: format!(
                    "llm-replay: no unconsumed round matches this request (model `{}`, \
                     {} round(s) in the transcript, {} consumed)",
                    req.model,
                    self.transcript.rounds.len(),
                    *cursor
                ),
                retryable: false,
                status: None,
                adapter: AdapterName::new(PLUGIN_NAME),
            })],
            // Lenient mode: an empty turn, terminated honestly.
            None => vec![Chunk::End {
                stop: bough_plugin_llm::StopReason::EndTurn,
            }],
        }
    }
}

#[async_trait::async_trait]
impl LlmAdapter for ReplayAdapter {
    fn name(&self) -> AdapterName {
        AdapterName::new(PLUGIN_NAME)
    }

    async fn start(&self, req: Arc<LlmRequest>, cancel: CancellationToken) -> LlmStream {
        if cancel.is_cancelled() {
            return Box::pin(futures::stream::once(async move {
                Chunk::Failed(LlmFailure {
                    kind: FailureKind::Cancelled,
                    message: "cancelled before the replayed round started".into(),
                    retryable: false,
                    status: None,
                    adapter: AdapterName::new(PLUGIN_NAME),
                })
            }));
        }
        let mut chunks = self.answer(&req);
        // A recorded round with no terminal chunk would violate the seam's invariant; the
        // transcript is data, so the adapter closes it rather than trusting the file.
        if !chunks.last().map(Chunk::is_terminal).unwrap_or(false) {
            chunks.push(Chunk::End {
                stop: bough_plugin_llm::StopReason::EndTurn,
            });
        }
        Box::pin(futures::stream::iter(chunks))
    }
}

/// The provider row. In the catalog, in NO bundle: the swap patches name it (§17 Phase 2).
pub struct ReplayPlugin;

#[async_trait::async_trait]
impl Plugin for ReplayPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ReplayConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["llm"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        match (&cfg.transcript, &cfg.rounds) {
            (None, None) => Err(bough_kernel::ConfigError::Rejected {
                detail: "llm-replay needs either `transcript:` or `rounds:`".to_string(),
            }),
            (Some(_), Some(_)) => Err(bough_kernel::ConfigError::Rejected {
                detail: "llm-replay takes `transcript:` or `rounds:`, not both".to_string(),
            }),
            _ => Ok(()),
        }
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        // §0.2: misconfiguration fails at the earliest resolvable point. A transcript file is I/O,
        // so it is read HERE and not in `validate`, and an unreadable one fails the row's load.
        let transcript = ReplayAdapter::load(&cfg)
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), anyhow::anyhow!(e)))?;
        let llm = ctx
            .get::<Llm>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        llm.adapter(
            &ctx,
            AdapterSpec {
                name: AdapterName::new(PLUGIN_NAME),
                matches: ModelMatch::parse(&cfg.models),
                adapter: Arc::new(ReplayAdapter::new(cfg.clone(), transcript)),
            },
        )
        .await?;
        Ok(())
    }
}

bough_kernel::register_plugin!(ReplayPlugin);
