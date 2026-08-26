//! Invariant: search answers IDENTICALLY to ledger-sqlite for the queries the conformance suite
//! uses — a case-insensitive token match over body + cites, ordered `seq DESC, traj ASC`
//! (P1-D19). That agreement, not FTS parity in general, is what Phase 1 needs.

use bough_plugin_ledger::{LedgerError, SearchHit, SearchQuery, Step};

use crate::store::MemoryStore;

/// [`bough_plugin_ledger::LedgerStore::search`].
pub fn search(store: &MemoryStore, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
    let terms = tokenize(&q.text);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let inner = store.inner.read();
    let mut hits: Vec<SearchHit> = Vec::new();
    for step in store.readable_many(&inner, &q.trajs)? {
        let hay = haystack(&step);
        let tokens = tokenize(&hay);
        // FTS5 joins bare terms with AND, so every term must be present as a whole token.
        if !terms.iter().all(|t| tokens.iter().any(|h| h == t)) {
            continue;
        }
        let snippet = snippet(&hay, &terms[0]);
        hits.push(SearchHit { step, snippet });
    }
    // P1-D19: `seq DESC, traj ASC`, not bm25 rank — rank is what the two providers cannot agree on.
    hits.sort_by(|a, b| {
        b.step
            .seq
            .cmp(&a.step.seq)
            .then_with(|| a.step.traj.cmp(&b.step.traj))
    });
    hits.truncate(q.limit);
    Ok(hits)
}

/// The two FTS columns of `steps_fts`, concatenated: `body` then `cites`.
fn haystack(step: &Step) -> String {
    let cites = serde_json::to_string(step.cites.as_ref()).unwrap_or_default();
    format!("{} {}", step.body, cites)
}

/// FTS5's default tokenizer, near enough for the agreement Phase 1 needs: fold to lowercase and
/// split on everything that is not alphanumeric.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// A window around the first occurrence of `term`, so a hit's snippet always contains what was
/// searched for.
fn snippet(hay: &str, term: &str) -> String {
    let lower = hay.to_lowercase();
    let Some(at) = lower.find(term) else {
        return hay.chars().take(120).collect();
    };
    let start = floor_boundary(hay, at.saturating_sub(40));
    let end = ceil_boundary(hay, (at + term.len() + 40).min(hay.len()));
    hay[start..end].to_string()
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizing_folds_case_and_splits_on_punctuation() {
        assert_eq!(tokenize("Hello, World-42!"), vec!["hello", "world", "42"]);
    }

    #[test]
    fn a_snippet_contains_the_term_it_matched() {
        let hay = "the quick brown fox jumps over the lazy dog and keeps going for a while";
        assert!(snippet(hay, "lazy").contains("lazy"));
    }
}
