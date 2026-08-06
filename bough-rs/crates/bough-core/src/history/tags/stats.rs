//! Tag popularity over the command-history memory (port of
//! `src/history/stats.ts`): the session-start priming note and the
//! per-directory profiles behind the mid-turn hints.
//!
//! Weighting, not raw counts: a tag's weight is its ACT-R base-level
//! activation, scaled by whether the command worked — frequency and recency
//! in one term, with recency decaying as a power law (`tag_weights` carries
//! the evidence). The decay runs here rather than in SQL so nothing depends
//! on the sqlite build carrying math functions.
//!
//! Cache discipline: the priming note goes into the VOLATILE prompt tier,
//! which is cached per session with a 1h TTL (`llm/client`). Recomputing it
//! per turn would change its text mid-session and bust that cache, so it is
//! memoized per session for the process lifetime ([`StatsMemo`]; the runner
//! uses the process-global [`stats_memo`], tests own their instances).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::errors::BoughError;
use crate::types::{CommandTagOpts, CommandTagRow, Db};

use super::record::{find_git_root, is_ref, repo_identity};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Rows older than this contribute under a tenth of a fresh use — not worth
/// reading.
const LOOKBACK_MS: i64 = 150 * DAY_MS;
const TOP_TAGS: usize = 10;

/// The power law of forgetting. ACT-R's base-level learning equation puts the
/// decay exponent at 0.5 and the whole cognitive-psychology literature has
/// left it there.
const DECAY_D: f64 = 0.5;
/// A floor on "how long ago", because `t^-d` diverges at zero and the note
/// must not be dominated by whatever ran ninety seconds ago. An hour is also
/// roughly the resolution the note has anyway — it is memoized per session,
/// so finer recency than this could not reach the model even if it were
/// computed.
const RECENCY_FLOOR_MS: i64 = 60 * 60 * 1000;

fn success_factor(exit_code: Option<i64>) -> f64 {
    match exit_code {
        Some(0) => 1.0,
        None => 0.5,
        Some(_) => 0.25,
    }
}

/// Aggregate rows into per-tag weights: **base-level activation**, the ACT-R
/// model of how available a memory is, applied to tags.
///
/// ```text
/// BLA_i = ln( Σ_j t_j^-d )     d = 0.5
/// ```
///
/// where `t_j` is how long ago the j-th use was. Frequency and recency in one
/// term, with recency decaying as a POWER law rather than an exponential one
/// (Kowald et al., *Long Time No See*, 2014 — the ACT-R power law beat the
/// exponential half-life this replaced on every dataset tested, and recency
/// mattered most in the NARROW folksonomy; bough is as narrow as one gets).
///
/// THE LOG IS DROPPED, deliberately. `ln` is monotone, so it cannot change
/// this ranking; ACT-R takes it because activation feeds a sigmoid retrieval
/// probability, which nothing here computes. And `rank_tags` MULTIPLIES this
/// weight by an idf factor — a log-scaled magnitude would make that product
/// meaningless (and can go negative, which would invert the boost).
///
/// `success_factor` is ours and stays: a tag attached to a command that
/// failed is weaker evidence about this project's vocabulary than one
/// attached to a command that worked.
pub fn tag_weights(rows: &[CommandTagRow], now: i64) -> HashMap<String, f64> {
    let mut weights: HashMap<String, f64> = HashMap::new();
    for r in rows {
        let elapsed = (now - r.ts).max(RECENCY_FLOOR_MS) as f64 / DAY_MS as f64;
        let w = success_factor(r.exit_code) * elapsed.powf(-DECAY_D);
        *weights.entry(r.tag.clone()).or_insert(0.0) += w;
    }
    weights
}

fn top(weights: &HashMap<String, f64>, limit: usize) -> Vec<String> {
    let mut entries: Vec<(&String, &f64)> = weights.iter().collect();
    entries.sort_by(|a, b| {
        b.1.partial_cmp(a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    entries
        .into_iter()
        .take(limit)
        .map(|(tag, _)| tag.clone())
        .collect()
}

/// One tag as the priming note ranks it. Exported for `bough tags`, which
/// shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct RankedTag {
    pub tag: String,
    /// Success × recency, the raw popularity.
    pub weight: f64,
    /// How many repos in the memory use it.
    pub repos: i64,
    /// `weight × idf` — what the order is actually by.
    pub score: f64,
}

/// What `rank_tags` contrasts against: how many distinct repos the memory
/// holds, and how many of them use each tag (`Db::tag_spread`).
#[derive(Clone, Debug, Default)]
pub struct TagSpread {
    pub repos: i64,
    pub by_tag: HashMap<String, i64>,
}

/// Rank tags by how much this project's OWN vocabulary they are, not by raw
/// use.
///
/// WHY NOT POPULARITY. The grammar is `tool:intent:subject`, and a popularity
/// ranking is dominated by the first two: `git`, `bun`, `rg`, `test` recur in
/// every project, while `composer` or `retention` recur only in the one they
/// belong to. The correction is inverse document frequency over REPOS:
/// `weight × ln(1 + N/n)`. A tag in every repo is damped, one in a single
/// repo is lifted. With one repo in the memory every idf is `ln 2` and the
/// order is exactly the popularity order — the honest answer when there is
/// nothing to contrast against, and no special case for a fresh install.
pub fn rank_tags(
    weights: &HashMap<String, f64>,
    spread: &TagSpread,
    limit: usize,
    uses: Option<&HashMap<String, i64>>,
) -> Vec<RankedTag> {
    let mut ranked: Vec<RankedTag> = weights
        .iter()
        // A WORD USED ONCE IS NOT YET VOCABULARY. 40% of this memory's coined
        // tags have exactly one use, and a list whose job is "the words to
        // reuse here" cannot be teaching one of them. DEMOTED, NOT DELETED —
        // the row keeps the tag, `tags show` still finds it, FTS still
        // indexes it. Absent `uses` (the per-directory hints), nothing is
        // demoted — those lists are already narrow and answer a different
        // question.
        .filter(|(tag, _)| uses.and_then(|u| u.get(*tag).copied()).unwrap_or(2) > 1)
        // REFERENCES NEVER RANK. `linear.eng-1234` lives in exactly one repo,
        // so the idf below hands it the maximum boost — and it accumulates
        // real weight, because a ticket is worked over many commands. The two
        // multiply, and the note would open every session by reciting last
        // week's ticket numbers instead of this project's words.
        .filter(|(tag, _)| !is_ref(tag))
        .map(|(tag, &weight)| {
            let repos = spread.by_tag.get(tag).copied().unwrap_or(1);
            RankedTag {
                tag: tag.clone(),
                weight,
                repos,
                score: weight * (1.0 + spread.repos as f64 / repos as f64).ln(),
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.tag.cmp(&b.tag))
    });
    ranked.truncate(limit);
    ranked
}

/// The workspace's memory scope: its enclosing checkout's identity, else its
/// path.
pub fn workspace_repo(workspace: &str) -> String {
    repo_identity(&find_git_root(workspace).unwrap_or_else(|| workspace.to_string()))
}

/// A scope's most-used tags — whole repo, or one directory of it.
///
/// A DIRECTORY hint stays on plain popularity: it answers "what has been done
/// in here", where the tool is part of the answer, and the set is already
/// narrow enough that the tool names do not crowd anything out.
fn top_tags(
    db: &dyn Db,
    repo: &str,
    now: i64,
    limit: usize,
    dir: Option<&str>,
) -> Result<Vec<String>, BoughError> {
    let rows = db.command_tag_rows(
        repo,
        CommandTagOpts {
            dir: dir.map(str::to_string),
            since_ts: Some(now - LOOKBACK_MS),
        },
    )?;
    Ok(top(&tag_weights(&rows, now), limit))
}

/// The workspace repo's tags as the priming note ranks them — see
/// [`rank_tags`].
pub fn top_repo_tags(
    db: &dyn Db,
    workspace: &str,
    now: i64,
    limit: usize,
) -> Result<Vec<String>, BoughError> {
    Ok(
        ranked_repo_tags(db, &workspace_repo(workspace), now, limit)?
            .into_iter()
            .map(|r| r.tag)
            .collect(),
    )
}

/// The same ranking with its arithmetic attached, for `bough tags` to show.
pub fn ranked_repo_tags(
    db: &dyn Db,
    repo: &str,
    now: i64,
    limit: usize,
) -> Result<Vec<RankedTag>, BoughError> {
    let since = now - LOOKBACK_MS;
    let rows = db.command_tag_rows(
        repo,
        CommandTagOpts {
            dir: None,
            since_ts: Some(since),
        },
    )?;
    let mut uses: HashMap<String, i64> = HashMap::new();
    for r in &rows {
        *uses.entry(r.tag.clone()).or_insert(0) += 1;
    }
    let (repos, by_tag) = db.tag_spread(Some(since))?;
    Ok(rank_tags(
        &tag_weights(&rows, now),
        &TagSpread { repos, by_tag },
        limit,
        Some(&uses),
    ))
}

// ---------------------------------------------------------------------------
// The session-start priming note
// ---------------------------------------------------------------------------

/// Cap per session — the first thing to cut if the hints read as noise.
const MAX_HINTS_PER_SESSION: usize = 4;

/// At the cap the map is CLEARED then re-inserted — wholesale, not eviction.
const MEMO_CAP: usize = 512;

#[derive(Default)]
struct HintState {
    seen_dirs: HashSet<String>,
    emitted: usize,
}

/// The per-session memos: note text, primed tag set, hint state. All three
/// are process-lifetime in production ([`stats_memo`]) and bounded at
/// [`MEMO_CAP`] entries, cleared wholesale at the cap.
#[derive(Default)]
pub struct StatsMemo {
    notes: Mutex<HashMap<String, Option<String>>>,
    primed: Mutex<HashMap<String, HashSet<String>>>,
    hints: Mutex<HashMap<String, HintState>>,
}

impl StatsMemo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test seam: reset the per-session memos.
    pub fn reset(&self) {
        if let Ok(mut m) = self.notes.lock() {
            m.clear();
        }
        if let Ok(mut m) = self.primed.lock() {
            m.clear();
        }
        if let Ok(mut m) = self.hints.lock() {
            m.clear();
        }
    }
}

fn remember<T>(map: &mut HashMap<String, T>, key: &str, value: T) {
    if map.len() >= MEMO_CAP {
        map.clear();
    }
    map.insert(key.to_string(), value);
}

/// The process-global memo the turn runner and the sessions snapshot share.
pub fn stats_memo() -> &'static StatsMemo {
    static MEMO: OnceLock<StatsMemo> = OnceLock::new();
    MEMO.get_or_init(StatsMemo::new)
}

/// Test seam over the global memo — the TS `resetStatsMemo`.
pub fn reset_stats_memo() {
    stats_memo().reset();
}

/// The volatile-tier note naming this project's popular tags, or None for a
/// project with no history yet (the static examples in prompt/shell.md are
/// the cold-start fallback). Frozen per session — see the module header.
pub fn tags_note_for(
    db: &dyn Db,
    memo: &StatsMemo,
    session_id: &str,
    workspace: &str,
    now: i64,
) -> Option<String> {
    if let Ok(notes) = memo.notes.lock() {
        if let Some(hit) = notes.get(session_id) {
            return hit.clone();
        }
    }
    let mut note: Option<String> = None;
    // Stats are a garnish; a failure here must not touch the turn — a db
    // error leaves the note None (and memoizes the None, same as TS).
    if let Ok(tags) = top_repo_tags(db, workspace, now, TOP_TAGS) {
        if let Ok(mut primed) = memo.primed.lock() {
            remember(&mut primed, session_id, tags.iter().cloned().collect());
        }
        if !tags.is_empty() {
            note = Some(format!(
                "This project's own tag vocabulary — the words it uses that other projects \
                 do not: {}. Reuse these when they fit; coin new ones freely when they do \
                 not, especially for the tool and the intent.",
                tags.join(", ")
            ));
        }
    }
    if let Ok(mut notes) = memo.notes.lock() {
        remember(&mut notes, session_id, note.clone());
    }
    note
}

/// The tag set the session was primed with; empty when priming never ran.
pub fn primed_tags(memo: &StatsMemo, session_id: &str) -> HashSet<String> {
    memo.primed
        .lock()
        .ok()
        .and_then(|m| m.get(session_id).cloned())
        .unwrap_or_default()
}

/// The primed tags as an ordered list, computing (and freezing) them when
/// this session has none yet — the TUI snapshot's view of the same memo the
/// prompt note uses, so the two surfaces cannot disagree within a session.
pub fn primed_tags_for(
    db: &dyn Db,
    memo: &StatsMemo,
    session_id: &str,
    workspace: &str,
    now: i64,
) -> Vec<String> {
    tags_note_for(db, memo, session_id, workspace, now);
    primed_tags(memo, session_id).into_iter().collect()
}

// ---------------------------------------------------------------------------
// Directory-triggered hints
// ---------------------------------------------------------------------------

/// Hint lines for directories the round newly touched — by `view()` reads or
/// by the paths its shell commands named — when a directory's tag profile
/// DIVERGES from what the session was already primed with. No divergence →
/// no line → no context bloat. Once per directory, at most 4 per session.
///
/// `abs_dirs` are ABSOLUTE. Each resolves to its own enclosing checkout, so
/// a session rooted at `~` that starts working on `~/repos/bough` gets THAT
/// repo's profile — the cross-repo case the workspace-scoped version was
/// blind to. The workspace repo's own root is skipped (its profile IS the
/// priming set); a foreign repo's root surfaces its whole-repo tags.
pub fn dir_tag_hints(
    db: &dyn Db,
    memo: &StatsMemo,
    session_id: &str,
    workspace: &str,
    abs_dirs: &[String],
    now: i64,
) -> Vec<String> {
    let primed = primed_tags(memo, session_id);
    let ws_root = find_git_root(workspace).unwrap_or_else(|| workspace.to_string());
    let ws_repo = repo_identity(&ws_root);
    let mut lines: Vec<String> = Vec::new();
    let Ok(mut hints) = memo.hints.lock() else {
        return lines;
    };
    if !hints.contains_key(session_id) {
        remember(&mut hints, session_id, HintState::default());
    }
    let state = hints.get_mut(session_id).expect("just inserted");
    for abs in abs_dirs {
        if state.emitted >= MAX_HINTS_PER_SESSION {
            break;
        }
        if !Path::new(abs).is_absolute() || state.seen_dirs.contains(abs) {
            continue;
        }
        // Seen is recorded even when no hint emits — once per directory, ever.
        state.seen_dirs.insert(abs.clone());
        let root = find_git_root(abs).unwrap_or_else(|| ws_root.clone());
        let repo = repo_identity(&root);
        let rel = match Path::new(abs).strip_prefix(&root) {
            Ok(r) => r.to_string_lossy().into_owned(),
            // Outside its own root — the TS `..`-escape skip.
            Err(_) => continue,
        };
        let at_root = rel.is_empty() || rel == ".";
        if repo == ws_repo && at_root {
            continue;
        }
        // Same contract as everything here: hints never hurt a round — a
        // failed lookup is a skipped dir.
        let Ok(fresh) = top_tags(db, &repo, now, 5, if at_root { None } else { Some(&rel) }) else {
            continue;
        };
        let fresh: Vec<String> = fresh.into_iter().filter(|t| !primed.contains(t)).collect();
        if fresh.is_empty() {
            continue;
        }
        state.emitted += 1;
        // Same-repo dirs label as the familiar relative path; a foreign repo
        // labels as its own location, home-abbreviated.
        let label = if repo == ws_repo {
            rel
        } else {
            match dirs::home_dir() {
                Some(h) => abs.replacen(&h.to_string_lossy().into_owned(), "~", 1),
                None => abs.clone(),
            }
        };
        lines.push(format!(
            "[history] tags previously used in {label}/: {} — run `bough tags show <tag>` \
             for the commands behind them",
            fresh.join(", ")
        ));
    }
    lines
}

// ---------------------------------------------------------------------------
// Tests — ported from src/history/stats.test.ts. The weighting tests are
// pure; the note/hint tests run over a real in-memory SqliteDb (the TS stub
// db's job — returning canned rows per (repo, dir) — is done by seeding real
// rows, which also exercises the pinned SQL underneath).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{Session, SessionKind};
    use crate::types::CommandRecord;

    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn row(tag: &str, ts: i64, exit_code: Option<i64>) -> CommandTagRow {
        CommandTagRow {
            tag: tag.to_string(),
            ts,
            exit_code,
        }
    }

    #[test]
    fn a_failing_commands_tag_weighs_a_quarter_of_a_passing_one() {
        let now = 1_000_000;
        let w = tag_weights(
            &[
                row("ok", now, Some(0)),
                row("bad", now, Some(1)),
                row("unknown", now, None),
            ],
            now,
        );
        // Ratios, not absolutes: the magnitude of a fresh use is the recency
        // floor's business, and the success factor must hold whatever that
        // constant is.
        assert!((w["bad"] / w["ok"] - 0.25).abs() < 1e-9);
        assert!((w["unknown"] / w["ok"] - 0.5).abs() < 1e-9);
    }

    #[test]
    fn recency_decays_as_a_power_law_so_doubling_the_age_costs_sqrt2() {
        let now = 400 * DAY;
        let w = tag_weights(
            &[
                row("d10", now - 10 * DAY, Some(0)),
                row("d20", now - 20 * DAY, Some(0)),
                row("d40", now - 40 * DAY, Some(0)),
            ],
            now,
        );
        // t^-0.5: every doubling of elapsed time is the same constant ratio.
        // An exponential half-life — what this used to be — would have buried
        // the 40-day tag an order of magnitude deeper.
        let r1 = w["d20"] / w["d10"];
        let r2 = w["d40"] / w["d20"];
        assert!(
            (r1 - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9,
            "20d/10d = {r1}"
        );
        assert!(
            (r2 - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9,
            "40d/20d = {r2}"
        );
    }

    #[test]
    fn frequency_and_recency_trade_off_four_old_uses_beat_one_recent_one() {
        let now = 400 * DAY;
        let mut rows: Vec<CommandTagRow> = (0..4)
            .map(|_| row("habit", now - 40 * DAY, Some(0)))
            .collect();
        rows.push(row("novelty", now - 8 * DAY, Some(0)));
        let w = tag_weights(&rows, now);
        // 4×40^-0.5 = 0.632 against 8^-0.5 = 0.354. Activation is a SUM over
        // uses, which is what keeps a long-standing habit visible against
        // whatever happened lately.
        assert!(
            w["habit"] > w["novelty"],
            "{} vs {}",
            w["habit"],
            w["novelty"]
        );
    }

    #[test]
    fn rank_tags_carries_the_arithmetic_it_sorted_by() {
        // `bough tags` shows these columns, so a user can see WHY the note
        // says what it says rather than being told to trust it.
        let weights: HashMap<String, f64> =
            [("git".to_string(), 8.0), ("composer".to_string(), 4.0)].into();
        let spread = TagSpread {
            repos: 12,
            by_tag: [("git".to_string(), 12), ("composer".to_string(), 1)].into(),
        };
        let ranked = rank_tags(&weights, &spread, 5, None);
        assert_eq!(
            ranked.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
            ["composer", "git"]
        );
        assert_eq!(ranked[0].repos, 1);
        assert_eq!(ranked[0].weight, 4.0);
        assert!(
            ranked[0].score > ranked[1].score,
            "the score is what the order is by"
        );
        // A tag the spread has never seen is treated as this project's alone,
        // not as ubiquitous — an unknown must not be damped into last place.
        let unknown = rank_tags(
            &[("new".to_string(), 1.0)].into(),
            &TagSpread {
                repos: 4,
                by_tag: HashMap::new(),
            },
            5,
            None,
        );
        assert_eq!(unknown[0].repos, 1);
    }

    #[test]
    fn references_never_rank_but_singleton_demotion_needs_uses() {
        let weights: HashMap<String, f64> = [
            ("linear.eng-1234".to_string(), 50.0),
            ("composer".to_string(), 1.0),
        ]
        .into();
        let spread = TagSpread {
            repos: 12,
            by_tag: HashMap::new(),
        };
        let ranked = rank_tags(&weights, &spread, 5, None);
        assert_eq!(
            ranked.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(),
            ["composer"]
        );
        // With `uses`, a once-used word is demoted; without, it is not.
        let uses: HashMap<String, i64> = [("composer".to_string(), 1)].into();
        assert!(rank_tags(&weights, &spread, 5, Some(&uses)).is_empty());
    }

    // ---- db-backed note + hints --------------------------------------------

    struct Fx {
        db: SqliteDb,
        ws: std::path::PathBuf,
    }

    impl Fx {
        fn new() -> Fx {
            let ws = std::env::temp_dir().join(format!("bough-stats-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&ws).unwrap();
            let db = SqliteDb::new(":memory:", DbOptions::default()).unwrap();
            db.create_session(Session {
                id: "sess".into(),
                title: "sess".into(),
                kind: SessionKind::Root,
                created_at: 1,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
            })
            .unwrap();
            Fx { db, ws }
        }

        fn ws(&self) -> String {
            self.ws.to_string_lossy().into_owned()
        }

        /// One history row: `repo` scoped, optionally attributed to a dir.
        fn seed(&self, repo: &str, tag: &str, ts: i64, dir: Option<&str>) {
            self.db
                .record_command(&CommandRecord {
                    session_id: "sess".into(),
                    ts,
                    repo: repo.to_string(),
                    cmd: format!("cmd-{tag}-{ts}"),
                    tags: tag.to_string(),
                    tag_list: vec![tag.to_string()],
                    dirs: dir.map(|d| vec![d.to_string()]).unwrap_or_default(),
                    exit_code: Some(0),
                    duration_ms: Some(1),
                    output_head: String::new(),
                    spill_path: None,
                    source: "live".into(),
                    message_id: None,
                })
                .unwrap();
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.ws);
        }
    }

    #[test]
    fn the_note_prefers_this_projects_own_words_over_the_tools_every_project_uses() {
        let f = Fx::new();
        let ws = f.ws();
        let now = 1_000 * DAY;
        // `git` is used twice as much as `composer` here — and in all twelve
        // repos the memory knows, which is what makes it a tool name rather
        // than this project's vocabulary.
        for _ in 0..8 {
            f.seed(&ws, "git", now, None);
            f.seed(&ws, "bun", now, None);
        }
        for _ in 0..4 {
            f.seed(&ws, "composer", now, None);
        }
        for _ in 0..3 {
            f.seed(&ws, "retention", now, None);
        }
        // Eleven other repos use git; eight of them also use bun.
        for i in 0..11 {
            f.seed(&format!("repo-{i}"), "git", now, None);
            if i < 8 {
                f.seed(&format!("repo-{i}"), "bun", now, None);
            }
        }
        assert_eq!(
            top_repo_tags(&f.db, &ws, now, 4).unwrap(),
            ["composer", "retention", "bun", "git"]
        );
    }

    #[test]
    fn with_one_repo_the_ranking_inverts_back_to_popularity() {
        // One repo means every idf is ln 2, which is the honest answer on a
        // fresh install and needs no special case.
        let f = Fx::new();
        let ws = f.ws();
        let now = 1_000 * DAY;
        for _ in 0..8 {
            f.seed(&ws, "git", now, None);
            f.seed(&ws, "bun", now, None);
        }
        for _ in 0..4 {
            f.seed(&ws, "composer", now, None);
        }
        assert_eq!(top_repo_tags(&f.db, &ws, now, 2).unwrap(), ["bun", "git"]);
    }

    #[test]
    fn tags_note_for_names_the_top_tags_once_and_freezes_per_session() {
        let f = Fx::new();
        let ws = f.ws();
        let now = 1_000 * DAY;
        // Two uses each: the note demotes a word used exactly once, which is
        // not yet vocabulary — see `rank_tags`.
        f.seed(&ws, "git", now, None);
        f.seed(&ws, "git", now - DAY, None);
        f.seed(&ws, "bun", now, None);
        f.seed(&ws, "bun", now - DAY, None);
        f.seed(&ws, "loner", now, None);
        let memo = StatsMemo::new();
        let first = tags_note_for(&f.db, &memo, "sess", &ws, now).expect("a note");
        assert!(first.contains("git") && first.contains("bun"), "{first}");
        assert!(
            !first.contains("loner"),
            "a singleton is demoted out of the note: {first}"
        );
        // A session's note never drifts, even when the stats underneath it
        // change.
        for _ in 0..10 {
            f.seed(&ws, "other", now, None);
            f.seed(&ws, "other", now - DAY, None);
        }
        let drifted = tags_note_for(&f.db, &memo, "sess", &ws, now);
        assert_eq!(drifted.as_deref(), Some(first.as_str()));
        // A fresh session sees the new stats — the freeze is per session.
        let fresh = tags_note_for(&f.db, &memo, "sess2", &ws, now).expect("a note");
        assert!(fresh.contains("other"));
    }

    #[test]
    fn tags_note_for_is_null_and_stays_null_for_a_project_with_no_history() {
        let f = Fx::new();
        let memo = StatsMemo::new();
        assert_eq!(tags_note_for(&f.db, &memo, "sess", &f.ws(), 1), None);
        assert_eq!(tags_note_for(&f.db, &memo, "sess", &f.ws(), 1), None);
    }

    #[test]
    fn a_directory_hints_only_when_its_profile_diverges_from_the_primed_set() {
        let f = Fx::new();
        let ws = f.ws();
        let now = 1_000 * DAY;
        f.seed(&ws, "bun", now, None);
        f.seed(&ws, "bun", now - DAY, None);
        f.seed(&ws, "psql", now, Some("migrations"));
        f.seed(&ws, "bun", now, Some("migrations"));
        f.seed(&ws, "bun", now, Some("src/tui"));
        let memo = StatsMemo::new();
        // Prime the session so the divergence rule has a baseline.
        tags_note_for(&f.db, &memo, "sess", &ws, now);
        let migrations = format!("{ws}/migrations");
        let tui = format!("{ws}/src/tui");
        let lines = dir_tag_hints(&f.db, &memo, "sess", &ws, &[migrations.clone(), tui], now);
        // migrations diverges (psql); src/tui is covered by the primed set —
        // silent.
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("migrations/") && lines[0].contains("psql"),
            "{}",
            lines[0]
        );
        assert!(
            !lines[0].contains("bun"),
            "already-primed tags never repeat in a hint"
        );
        assert!(
            lines[0].contains("run `bough tags show <tag>` for the commands behind them"),
            "{}",
            lines[0]
        );
        // Once per directory, ever.
        assert_eq!(
            dir_tag_hints(&f.db, &memo, "sess", &ws, &[migrations], now).len(),
            0
        );
    }

    #[test]
    fn hints_stop_at_the_per_session_cap() {
        let f = Fx::new();
        let ws = f.ws();
        let now = 1_000 * DAY;
        let mut dirs: Vec<String> = Vec::new();
        for i in 0..6 {
            f.seed(&ws, &format!("t{i}"), now, Some(&format!("d{i}")));
            dirs.push(format!("{ws}/d{i}"));
        }
        let memo = StatsMemo::new();
        let lines = dir_tag_hints(&f.db, &memo, "sess", &ws, &dirs, now);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn touching_a_foreign_checkout_surfaces_that_repos_own_profile() {
        // `home` plays the workspace; a separate checkout lives at
        // home/repos/proj. Only the foreign checkout's identity has history.
        let f = Fx::new();
        let home = f.ws();
        let proj = std::path::Path::new(&home).join("repos/proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        let proj = proj.to_string_lossy().into_owned();
        let now = 1_000 * DAY;
        f.seed(&proj, "docs:read", now, None);
        let memo = StatsMemo::new();
        // The workspace (home) has no history of its own — priming is empty.
        tags_note_for(&f.db, &memo, "sess", &home, now);
        let lines = dir_tag_hints(
            &f.db,
            &memo,
            "sess",
            &home,
            std::slice::from_ref(&proj),
            now,
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("repos/proj/"), "{}", lines[0]);
        assert!(lines[0].contains("docs:read"), "{}", lines[0]);
        // The workspace's OWN root never hints — its profile is the priming
        // set.
        assert_eq!(
            dir_tag_hints(
                &f.db,
                &memo,
                "sess",
                &home,
                std::slice::from_ref(&home),
                now
            )
            .len(),
            0
        );
    }

    #[test]
    fn the_memo_cap_clears_wholesale_then_reinserts() {
        let memo = StatsMemo::new();
        {
            let mut notes = memo.notes.lock().unwrap();
            for i in 0..MEMO_CAP {
                remember(&mut notes, &format!("s{i}"), Some(format!("n{i}")));
            }
            assert_eq!(notes.len(), MEMO_CAP);
            // The insert AT the cap clears the whole map first — wholesale,
            // not eviction.
            remember(&mut notes, "overflow", Some("v".to_string()));
            assert_eq!(notes.len(), 1);
            assert!(notes.contains_key("overflow"));
        }
    }
}
