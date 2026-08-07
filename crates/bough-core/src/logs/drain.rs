//! Port of `src/logs/drain.ts` — stage two of clustering: group logtypes that
//! masking could not make identical.
//!
//! THE ALGORITHM (Drain, He et al., ICWS 2017). A fixed-depth tree whose first
//! level is token COUNT and whose next few levels are the leading tokens. Lines
//! that reach the same leaf are compared to the handful of templates already
//! there, and one is chosen if it agrees on enough positions.
//!
//! ONE PASS, NO SECOND LOOK. A cluster is created or updated the moment a line
//! arrives and is never revisited — which is what lets the whole pipeline
//! stream, and which means templates depend on ARRIVAL ORDER. Counts and
//! statistics are unaffected; only which positions generalized first can differ.
//!
//! BOUNDED, AND HONEST WHEN THE BOUND BINDS. Clusters are capped and evicted
//! least-recently-used, and eviction is reported rather than absorbed.
//!
//! Pure: no clock, no filesystem, no randomness.
//!
//! PORT NOTE. The TS holds live `Cluster` objects in a leaf array AND in an
//! insertion-ordered `Map` used as the LRU queue, mutating `tokens` in place
//! through both. Rust gets the same aliasing by keeping clusters in one
//! `HashMap<id, Cluster>` arena that leaves reference by id, and by ordering the
//! LRU with a `BTreeMap<seq, id>` — `delete + reinsert` on a JS Map is O(1) and
//! a `VecDeque` remove would have been O(n) per line.

use std::collections::{BTreeMap, HashMap};

/// A group of lines the tree considers the same statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub id: u32,
    /// Template tokens. A position that generalized holds `WILDCARD`.
    pub tokens: Vec<String>,
    pub count: u64,
}

/// The token that marks a position whose value varies in a way masking could
/// not type.
pub const WILDCARD: &str = "<*>";

/// Tunables. Defaults are the paper's, except the cap, which is ours.
#[derive(Debug, Clone, Copy)]
pub struct DrainOptions {
    /// Fraction of positions that must agree for a line to join a template.
    /// 0.4 is the paper's default and it survives contact with real logs.
    pub threshold: f64,
    /// Total tree depth, counted the paper's way: root, the token-count level,
    /// the token levels, and the leaf. The number of LEADING TOKENS actually
    /// indexed is `depth - 2` — an off-by-two worth stating, because every token
    /// consumed as an index is a token that can never generalize.
    pub depth: usize,
    /// Distinct children before a node stops splitting and funnels the rest to
    /// one wildcard child.
    pub max_children: usize,
    /// Clusters held before least-recently-used eviction begins.
    pub max_clusters: usize,
}

impl Default for DrainOptions {
    fn default() -> Self {
        Self {
            threshold: 0.4,
            depth: 4,
            max_children: 100,
            max_clusters: 10000,
        }
    }
}

#[derive(Debug, Default)]
struct Node {
    children: HashMap<String, usize>,
    clusters: Vec<u32>,
}

/// A token is used as a tree index only if it is stable enough to index on.
/// Two things disqualify it, and BOTH are needed: it contains a digit (the
/// paper's rule — `worker-3` is a value wearing a word's clothes), or it
/// contains a placeholder (this pipeline's addition, because masking runs first
/// and a masked token like `[<hex>]` has no digit yet is MORE variable, not
/// less).
fn index_key(token: &str) -> String {
    if token.chars().any(|c| c.is_ascii_digit()) || token.contains('<') {
        WILDCARD.to_string()
    } else {
        token.to_string()
    }
}

pub struct Drain {
    nodes: Vec<Node>,
    root: HashMap<usize, usize>,
    threshold: f64,
    /// Leading tokens used as tree index: `depth - 2`, per the paper's depth
    /// convention.
    index_tokens: usize,
    max_children: usize,
    max_clusters: usize,
    clusters: HashMap<u32, Cluster>,
    /// The LRU queue: `seq -> id`, oldest first. Stands in for the TS's
    /// insertion-ordered Map.
    lru: BTreeMap<u64, u32>,
    seq_of: HashMap<u32, u64>,
    next_seq: u64,
    next_id: u32,
    evicted: u64,
    /// Ids the cap forced out since the last drain, so a caller can drop their
    /// statistics too. The TS passes an `onEvict` callback; a Rust closure held
    /// by the struct would borrow the Analyzer that owns it, so the ids are
    /// queued and the caller drains them after each `add`.
    evictions: Vec<u32>,
}

impl Default for Drain {
    fn default() -> Self {
        Self::new(DrainOptions::default())
    }
}

impl Drain {
    pub fn new(opts: DrainOptions) -> Self {
        Self {
            nodes: Vec::new(),
            root: HashMap::new(),
            threshold: opts.threshold,
            // Floor of one so a caller passing `depth: 2` still gets a usable
            // tree rather than a single leaf holding every line of a length.
            index_tokens: opts.depth.saturating_sub(2).max(1),
            max_children: opts.max_children,
            max_clusters: opts.max_clusters,
            clusters: HashMap::new(),
            lru: BTreeMap::new(),
            seq_of: HashMap::new(),
            next_seq: 0,
            next_id: 1,
            evicted: 0,
            evictions: Vec::new(),
        }
    }

    /// True once the cap forced a cluster out and counts stopped being complete.
    pub fn truncated(&self) -> bool {
        self.evicted > 0
    }

    /// Cluster ids evicted since this was last called. `Analyzer` uses it to
    /// drop the pattern's accumulators — otherwise its map becomes the
    /// unbounded thing the cap exists to prevent.
    pub fn take_evictions(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.evictions)
    }

    /// Every live cluster, most frequent first. The underlying order is the LRU
    /// order and the sort is stable, matching the TS `[...byId.values()].sort`.
    pub fn clusters(&self) -> Vec<Cluster> {
        let mut out: Vec<Cluster> = self
            .lru
            .values()
            .map(|id| self.clusters[id].clone())
            .collect();
        out.sort_by_key(|c| std::cmp::Reverse(c.count));
        out
    }

    /// Add one masked line's tokens; return the cluster it belongs to.
    ///
    /// The returned value is a SNAPSHOT, not the live cluster the TS hands back
    /// — its `tokens` would otherwise mutate under the caller. Statistics must
    /// be keyed on `id` and on token POSITION regardless, which is exactly what
    /// `stats.rs` does, so the snapshot changes nothing.
    pub fn add(&mut self, tokens: &[String]) -> Cluster {
        let leaf = self.descend(tokens);
        if let Some(id) = self.best_match(leaf, tokens) {
            {
                let cluster = self.clusters.get_mut(&id).expect("matched cluster exists");
                // Generalize in place: positions that disagree become wildcards,
                // permanently. A template only ever loses specificity, which is
                // what makes one pass enough.
                for i in 0..cluster.tokens.len() {
                    let incoming = tokens.get(i).map(String::as_str);
                    if Some(cluster.tokens[i].as_str()) != incoming && cluster.tokens[i] != WILDCARD
                    {
                        cluster.tokens[i] = WILDCARD.to_string();
                    }
                }
                cluster.count += 1;
            }
            self.touch(id);
            return self.clusters[&id].clone();
        }
        let id = self.next_id;
        self.next_id += 1;
        let created = Cluster {
            id,
            tokens: tokens.to_vec(),
            count: 1,
        };
        self.nodes[leaf].clusters.push(id);
        self.clusters.insert(id, created.clone());
        self.touch(id);
        self.evict_if_needed(leaf);
        created
    }

    fn new_node(&mut self) -> usize {
        self.nodes.push(Node::default());
        self.nodes.len() - 1
    }

    /// Walk to the leaf for these tokens, creating nodes as needed.
    fn descend(&mut self, tokens: &[String]) -> usize {
        // Token count first. Two lines of different length are never one
        // statement, so this partition is free precision.
        let mut current = match self.root.get(&tokens.len()) {
            Some(idx) => *idx,
            None => {
                let idx = self.new_node();
                self.root.insert(tokens.len(), idx);
                idx
            }
        };
        let levels = self.index_tokens.min(tokens.len());
        for token in tokens.iter().take(levels) {
            let key = index_key(token);
            if let Some(next) = self.nodes[current].children.get(&key) {
                current = *next;
                continue;
            }
            // Past the fan-out limit, everything new shares one wildcard child
            // rather than growing the tree without bound.
            if self.nodes[current].children.len() >= self.max_children {
                if let Some(next) = self.nodes[current].children.get(WILDCARD) {
                    current = *next;
                    continue;
                }
                let idx = self.new_node();
                self.nodes[current]
                    .children
                    .insert(WILDCARD.to_string(), idx);
                current = idx;
                continue;
            }
            let idx = self.new_node();
            self.nodes[current].children.insert(key, idx);
            current = idx;
        }
        current
    }

    /// The leaf's best template, if any clears the threshold.
    ///
    /// Similarity is the fraction of positions holding the identical token. A
    /// wildcard position counts as agreement for NEITHER side: it is already
    /// known to vary, so crediting it would let a template that has generalized
    /// twice absorb anything of the right length. Ties break toward the template
    /// with fewer wildcards.
    fn best_match(&self, leaf: usize, tokens: &[String]) -> Option<u32> {
        let mut best: Option<u32> = None;
        let mut best_sim = -1.0f64;
        let mut best_wildcards = usize::MAX;
        for id in &self.nodes[leaf].clusters {
            let c = &self.clusters[id];
            let mut same = 0usize;
            let mut wildcards = 0usize;
            for (i, t) in c.tokens.iter().enumerate() {
                if t == WILDCARD {
                    wildcards += 1;
                } else if tokens.get(i) == Some(t) {
                    same += 1;
                }
            }
            let sim = if c.tokens.is_empty() {
                1.0
            } else {
                same as f64 / c.tokens.len() as f64
            };
            if sim > best_sim || (sim == best_sim && wildcards < best_wildcards) {
                best = Some(*id);
                best_sim = sim;
                best_wildcards = wildcards;
            }
        }
        if best_sim >= self.threshold {
            best
        } else {
            None
        }
    }

    /// Move a cluster to the back of the LRU queue.
    fn touch(&mut self, id: u32) {
        if let Some(old) = self.seq_of.remove(&id) {
            self.lru.remove(&old);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.lru.insert(seq, id);
        self.seq_of.insert(id, seq);
    }

    /// Drop the least recently touched cluster once the cap is exceeded.
    ///
    /// The victim is also spliced out of its leaf. Skipping that would leave a
    /// dead template in the candidate list forever — still matched against,
    /// still generalizing, but no longer counted anywhere, which corrupts every
    /// later line that resembles it.
    fn evict_if_needed(&mut self, leaf_of_new: usize) {
        while self.clusters.len() > self.max_clusters {
            let Some((&seq, &victim)) = self.lru.iter().next() else {
                return;
            };
            self.lru.remove(&seq);
            self.seq_of.remove(&victim);
            self.clusters.remove(&victim);
            self.evicted += 1;
            self.unlink(victim, leaf_of_new);
            self.evictions.push(victim);
        }
    }

    /// Remove a cluster from whichever leaf holds it.
    fn unlink(&mut self, victim: u32, hint: usize) {
        if let Some(at) = self.nodes[hint]
            .clusters
            .iter()
            .position(|id| *id == victim)
        {
            self.nodes[hint].clusters.remove(at);
            return;
        }
        // The victim is almost never in the leaf just written to, so fall back
        // to a walk. Eviction is rare by construction.
        for node in self.nodes.iter_mut() {
            if let Some(at) = node.clusters.iter().position(|id| *id == victim) {
                node.clusters.remove(at);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Port of `src/logs/drain.test.ts`. "What is asserted here is behaviour
    //! under the variability masking cannot type — hostnames, error strings,
    //! exception classes — because that is the only thing this stage exists to
    //! handle."
    use super::*;

    fn tok(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn identical_lines_form_one_cluster() {
        let mut d = Drain::default();
        for _ in 0..5 {
            d.add(&tok("server started on port <int>"));
        }
        let cs = d.clusters();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].count, 5);
    }

    #[test]
    fn a_varying_word_generalizes_to_a_wildcard() {
        let mut d = Drain::default();
        d.add(&tok("connect to db-primary failed"));
        d.add(&tok("connect to db-replica failed"));
        let cs = d.clusters();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].tokens, vec!["connect", "to", WILDCARD, "failed"]);
        assert_eq!(cs[0].count, 2);
    }

    #[test]
    fn lines_of_different_length_never_merge() {
        let mut d = Drain::default();
        d.add(&tok("a b c d e"));
        d.add(&tok("a b c d e f"));
        assert_eq!(d.clusters().len(), 2);
    }

    #[test]
    fn unrelated_lines_of_equal_length_stay_apart() {
        let mut d = Drain::default();
        d.add(&tok("user alice logged in"));
        d.add(&tok("disk sda3 nearly full"));
        assert_eq!(d.clusters().len(), 2);
    }

    #[test]
    fn a_template_only_ever_loses_specificity() {
        let mut d = Drain::default();
        d.add(&tok("job <int> on host alpha done"));
        d.add(&tok("job <int> on host beta done"));
        let after = d.clusters()[0].tokens.clone();
        d.add(&tok("job <int> on host alpha done"));
        assert_eq!(
            d.clusters()[0].tokens,
            after,
            "a repeat re-specialized the template"
        );
    }

    #[test]
    fn a_wildcard_position_credits_neither_side_of_the_similarity_test() {
        let mut d = Drain::new(DrainOptions {
            threshold: 0.6,
            ..Default::default()
        });
        d.add(&tok("a b c d e"));
        d.add(&tok("a b c d X")); // 4/5 = 0.8, merges; position 4 generalizes
        d.add(&tok("q r s t u")); // shares nothing; must not join
        assert_eq!(d.clusters().len(), 2);
    }

    #[test]
    fn a_digit_bearing_token_does_not_index_the_tree() {
        let mut d = Drain::default();
        for i in 0..20 {
            d.add(&tok(&format!("worker-{i} finished cleanly now")));
        }
        let cs = d.clusters();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].count, 20);
        assert_eq!(cs[0].tokens[0], WILDCARD);
    }

    #[test]
    fn the_threshold_is_respected_in_both_directions() {
        let mut strict = Drain::new(DrainOptions {
            threshold: 0.9,
            ..Default::default()
        });
        strict.add(&tok("a b c d"));
        strict.add(&tok("a b X Y"));
        assert_eq!(
            strict.clusters().len(),
            2,
            "0.5 similarity cleared a 0.9 threshold"
        );

        let mut loose = Drain::new(DrainOptions {
            threshold: 0.4,
            ..Default::default()
        });
        loose.add(&tok("a b c d"));
        loose.add(&tok("a b X Y"));
        assert_eq!(
            loose.clusters().len(),
            1,
            "0.5 similarity failed a 0.4 threshold"
        );
    }

    #[test]
    fn node_fan_out_is_capped_without_losing_lines() {
        let mut d = Drain::new(DrainOptions {
            max_children: 5,
            ..Default::default()
        });
        for i in 0..50 {
            let letter = (b'a' + (i % 26) as u8) as char;
            d.add(&tok(&format!("verb{letter}{i} noun thing here")));
        }
        let total: u64 = d.clusters().iter().map(|c| c.count).sum();
        assert_eq!(total, 50, "lines went missing past the fan-out limit");
    }

    #[test]
    fn the_cluster_cap_evicts_least_recently_used_and_says_so() {
        let mut d = Drain::new(DrainOptions {
            max_clusters: 3,
            ..Default::default()
        });
        d.add(&tok("alpha one two three"));
        d.add(&tok("bravo four five six"));
        d.add(&tok("charlie seven eight nine"));
        assert!(!d.truncated());
        d.add(&tok("delta ten eleven twelve"));

        assert!(d.truncated(), "eviction happened but was not reported");
        assert_eq!(d.clusters().len(), 3);
        let evicted = d.take_evictions();
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0], 1, "evicted the wrong cluster");
    }

    #[test]
    fn touching_a_cluster_spares_it_from_eviction() {
        let mut d = Drain::new(DrainOptions {
            max_clusters: 2,
            ..Default::default()
        });
        let alpha = d.add(&tok("alpha one two three")).id;
        let bravo = d.add(&tok("bravo four five six")).id;
        d.add(&tok("alpha one two three")); // alpha is now the most recent
        d.add(&tok("charlie seven eight nine"));
        assert_eq!(d.take_evictions(), vec![bravo]);
        assert!(d.clusters().iter().any(|c| c.id == alpha));
    }

    #[test]
    fn an_evicted_cluster_stops_being_matched_against() {
        let mut d = Drain::new(DrainOptions {
            max_clusters: 2,
            ..Default::default()
        });
        d.add(&tok("alpha one two three"));
        d.add(&tok("bravo four five six"));
        d.add(&tok("charlie seven eight nine")); // evicts alpha
        d.add(&tok("alpha one two three")); // must create fresh
        let alpha: Vec<Cluster> = d
            .clusters()
            .into_iter()
            .filter(|c| c.tokens[0] == "alpha")
            .collect();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].count, 1, "line joined an evicted cluster");
    }

    #[test]
    fn an_empty_token_list_does_not_crash_the_tree() {
        let mut d = Drain::default();
        d.add(&[]);
        d.add(&[]);
        assert_eq!(d.clusters().len(), 1);
        assert_eq!(d.clusters()[0].count, 2);
    }

    #[test]
    fn a_masked_token_does_not_index_the_tree_either() {
        // `[<hex>]` has no digit, so a digit-only rule indexes it literally
        // while the same statement's shorter ids key as wildcard. The two never
        // meet, and one log statement silently becomes two patterns.
        let mut d = Drain::default();
        d.add(&tok("ERROR [<hex>] connect to <ipv4> failed"));
        d.add(&tok("ERROR [a1b2c3d] connect to <ipv4> failed"));
        assert_eq!(d.clusters().len(), 1);
        assert_eq!(d.clusters()[0].count, 2);
    }
}
