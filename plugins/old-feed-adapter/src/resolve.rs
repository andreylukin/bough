//! Invariant: DEFAULTING IS AN EXPLICIT STEP (§0.2 — "defaulting is an explicit `resolve(request)
//! -> Spec` step in the owning provider, never a hidden `?? default` inside `run()`").
//!
//! `PrimingQuery` and `NoteQuery` are what a CALLER asks for; a `PrimingSpec` is what this row
//! will actually do. `limit: 0` used to mean "use the row's `priming_limit`" through an inline
//! `if q.limit == 0 { .. }` in the middle of `prime()`, which made a sentinel load-bearing and
//! undocumented at the type. It is spelled out here, once, and unit-tested.

use crate::{NoteQuery, OldFeedConfig, PrimingQuery};

/// What `prime` / `notes` will actually run. Every field is decided; nothing here is a sentinel.
#[derive(Clone, Debug, PartialEq)]
pub struct PrimingSpec {
    pub repo: Option<String>,
    pub tags: Vec<String>,
    pub contains: Option<String>,
    pub limit: usize,
}

/// PURE: a command-memory request ⇒ what will be run.
///
/// `limit: 0` is the caller saying "the row decides", which is `cfg.priming_limit`. `validate`
/// refuses a `priming_limit` of zero, so the resolved limit is always at least one.
pub fn resolve_priming(q: &PrimingQuery, cfg: &OldFeedConfig) -> PrimingSpec {
    PrimingSpec {
        repo: q.repo.clone(),
        tags: q.tags.clone(),
        contains: q.contains.clone(),
        limit: if q.limit == 0 {
            cfg.priming_limit
        } else {
            q.limit
        },
    }
}

/// PURE: a notes request ⇒ what will be run. Same rule, one place.
pub fn resolve_notes(q: &NoteQuery, cfg: &OldFeedConfig) -> PrimingSpec {
    PrimingSpec {
        repo: None,
        tags: Vec::new(),
        contains: q.contains.clone(),
        limit: if q.limit == 0 {
            cfg.priming_limit
        } else {
            q.limit
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OldFeedConfig {
        OldFeedConfig {
            jungler_db: "/nowhere/jungler.db".into(),
            bough_db: "/nowhere/bough.db".into(),
            state_db: "/nowhere/state.db".into(),
            poll_ms: 30_000,
            batch: 200,
            deliver_to: "sol".to_string(),
            priming_limit: 40,
            tier1: true,
        }
    }

    #[test]
    fn a_caller_that_names_no_limit_gets_the_rows_own() {
        let spec = resolve_priming(&PrimingQuery::default(), &cfg());
        assert_eq!(spec.limit, 40, "`limit: 0` means the row decides");
        assert_eq!(resolve_notes(&NoteQuery::default(), &cfg()).limit, 40);
    }

    #[test]
    fn a_caller_that_names_a_limit_keeps_it() {
        let q = PrimingQuery {
            repo: Some("bough".to_string()),
            tags: vec!["git".to_string()],
            contains: Some("rebase".to_string()),
            limit: 7,
        };
        let spec = resolve_priming(&q, &cfg());
        assert_eq!(spec.limit, 7);
        assert_eq!(spec.repo.as_deref(), Some("bough"));
        assert_eq!(spec.tags, vec!["git".to_string()]);
        assert_eq!(spec.contains.as_deref(), Some("rebase"));
    }
}
