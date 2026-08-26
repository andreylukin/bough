//! Invariant: adapter selection is an explicit `resolve(model) -> adapter` (§0.2). Most specific
//! wins; a tie is an error naming both adapters, never a silent last-wins.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::ids::AdapterName;
use crate::request::LlmRequest;
use crate::stream::LlmStream;

/// One registered adapter.
#[derive(Clone)]
pub struct AdapterSpec {
    pub name: AdapterName,
    pub matches: ModelMatch,
    pub adapter: Arc<dyn LlmAdapter>,
}

/// Which models an adapter claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelMatch {
    Exact(String),
    Prefix(String),
    Any,
}

impl ModelMatch {
    /// `Exact` 2 > `Prefix` 1 > `Any` 0. Two matches of equal specificity are a tie.
    pub fn specificity(&self) -> u8 {
        match self {
            ModelMatch::Exact(_) => 2,
            ModelMatch::Prefix(_) => 1,
            ModelMatch::Any => 0,
        }
    }

    /// Whether this match claims `model`.
    ///
    /// WP-1.
    pub fn claims(&self, _model: &str) -> bool {
        todo!("WP-1: Exact == , Prefix starts_with, Any always")
    }

    /// Parse the bundle spelling: `"*"`, `"claude-*"`, `"claude-haiku-4-5-20251001"`.
    ///
    /// WP-1.
    pub fn parse(_s: &str) -> ModelMatch {
        todo!("WP-1: parse the config spelling of a match")
    }
}

/// What a model provider does.
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync + 'static {
    fn name(&self) -> AdapterName;
    /// Never returns `Err`: a failure is the stream's terminal `Chunk::Failed` (§12).
    async fn start(&self, req: Arc<LlmRequest>, cancel: CancellationToken) -> LlmStream;
}
