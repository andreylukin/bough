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
    pub fn claims(&self, model: &str) -> bool {
        match self {
            ModelMatch::Exact(m) => m == model,
            ModelMatch::Prefix(p) => model.starts_with(p),
            ModelMatch::Any => true,
        }
    }

    /// Parse the bundle spelling: `"*"`, `"claude-*"`, `"claude-haiku-4-5-20251001"`.
    ///
    /// The ONE place a config string becomes a match, so `"*"` cannot mean `Exact("*")` in one row
    /// and `Any` in another.
    pub fn parse(s: &str) -> ModelMatch {
        match s.strip_suffix('*') {
            None => ModelMatch::Exact(s.to_string()),
            Some("") => ModelMatch::Any,
            Some(prefix) => ModelMatch::Prefix(prefix.to_string()),
        }
    }
}

/// What a model provider does.
#[async_trait::async_trait]
pub trait LlmAdapter: Send + Sync + 'static {
    fn name(&self) -> AdapterName;
    /// Never returns `Err`: a failure is the stream's terminal `Chunk::Failed` (§12).
    async fn start(&self, req: Arc<LlmRequest>, cancel: CancellationToken) -> LlmStream;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_spelling_parses_to_one_match_each() {
        assert_eq!(ModelMatch::parse("*"), ModelMatch::Any);
        assert_eq!(
            ModelMatch::parse("claude-*"),
            ModelMatch::Prefix("claude-".into())
        );
        assert_eq!(
            ModelMatch::parse("claude-haiku-4-5-20251001"),
            ModelMatch::Exact("claude-haiku-4-5-20251001".into())
        );
    }

    #[test]
    fn specificity_orders_exact_over_prefix_over_any() {
        assert!(
            ModelMatch::Exact("m".into()).specificity()
                > ModelMatch::Prefix("m".into()).specificity()
        );
        assert!(ModelMatch::Prefix("m".into()).specificity() > ModelMatch::Any.specificity());
    }

    #[test]
    fn claims_follows_the_shape() {
        let m = "claude-haiku-4-5-20251001";
        assert!(ModelMatch::Any.claims(m));
        assert!(ModelMatch::Prefix("claude-".into()).claims(m));
        assert!(!ModelMatch::Prefix("gpt-".into()).claims(m));
        assert!(ModelMatch::Exact(m.into()).claims(m));
        assert!(!ModelMatch::Exact("claude-opus-5".into()).claims(m));
    }
}
