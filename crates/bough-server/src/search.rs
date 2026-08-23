//! Keyword search over transcripts (port of `src/server/search.ts`) — the
//! whole of cross-session recall: SQLite FTS, no embeddings, no vector index.
//!
//! THE INVARIANT THIS MODULE HOLDS: **the search index is never load-bearing.**
//! A failure to index must never fail the write that triggered it. Indexing
//! runs on the insert path — sessions, the turn runner, branches, agents and
//! schedules all call `db.index_message` as they persist — which is what keeps
//! the index current with no background job to babysit. The price of being on
//! that path is that a broken `messages_fts` becomes a broken
//! `POST /sessions/:id/messages`, and losing a user's message to a
//! search-index error is not a trade anyone would take.
//!
//! [`SearchSafeDb`] is where that is paid, once, at the seam: it wraps the
//! `Db` handed to the app so `index_message` reports and swallows instead of
//! failing. Most call sites already guard themselves; wrapping is what makes
//! the guarantee hold for the one that does not, and for every future one,
//! without a rule everybody has to remember.
//!
//! WHAT A SWALLOWED ERROR COSTS, AND WHY THE COUNTER EXISTS. Silent
//! degradation is the bad failure mode for search: results that are quietly
//! missing look exactly like results that do not exist, and nothing ever
//! repairs them, because the write they belonged to is long gone. So the
//! wrapper counts what it swallowed and `GET /search` says so on every
//! response that follows, pointing at `POST /search/reindex` — the repair. A
//! search index is the one subsystem allowed to fail quietly; it is not
//! allowed to fail invisibly.
//!
//! QUERY SYNTAX IS THE USER'S, NOT OURS. The query goes to FTS5 verbatim, so
//! `"a phrase"`, `OR`, `NOT`, `NEAR` and `pref*` all work as documented. Bare
//! words are FTS5's implicit AND. Punctuation, though, is FTS5 syntax —
//! `what's up` and `foo-bar` are hard errors, not zero-result searches — so a
//! query that fails to parse is retried once with every whitespace-separated
//! chunk quoted into a phrase, and the response reports the rewrite in
//! `effectiveQuery`. Failing an ordinary human query with a parser message
//! would be indefensible; rewriting one without saying so would be worse.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use bough_core::errors::{BoughError, ErrorKind};
use bough_core::schema::parts::{
    is_collapsed_kind, Message, Part, Role, Schedule, Session, SessionKind, Turn, TurnStatus,
    Usage, WorkflowAgent, WorkflowRun,
};
use bough_core::schema::requests::SearchQuery;
use bough_core::types::{
    Clock, CommandRecord, CommandTagOpts, CommandTagRow, Db, IndexHealth, NoteAuthor, NoteLogRow,
    NoteRow, PriorFailures, RecalledCommand, RecentFailure, SearchHit, SectionRevision, SectionRow,
    SectionWrite, SessionRuntime, StateEntry, TagDiversityDay, TaggedCommand, TurnPatch,
    UsageTotals, WorkflowAgentPatch, WorkflowPatch,
};

use crate::http::{handler, json, Handler};

// ---- shapes -----------------------------------------------------------------

/// Hits per page when the caller does not say. Small: this is a picker, not an
/// export.
pub const DEFAULT_LIMIT: i64 = 20;
/// The ceiling `schema/requests.rs` also enforces on the wire.
pub const MAX_LIMIT: i64 = 200;

/// What an empty search says. One sentence of syntax, because the alternative
/// a user meets first is a validation issue about a zero-length string —
/// technically the same fact, useless as an answer.
const NEEDS_A_QUERY: &str = "search needs a query — GET /search?q=<words>. Bare words are \
ANDed; quote a phrase as \"like this\"; OR, NOT, NEAR and pref* work too.";

/// One hit, with enough around it to be worth rendering.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultHit {
    pub message_id: String,
    pub session_id: String,
    /// The owning session's title — what a human recognizes the conversation by.
    pub title: String,
    pub kind: SessionKind,
    /// True when the session surfaces only under `originId`.
    pub collapsed: bool,
    /// Where to drill in from, for a collapsed session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
    /// user | supervisor | system — "did I say this, or did the agent?"
    pub role: Role,
    /// The matched excerpt, FTS snippet markers already resolved.
    pub snippet: String,
    /// Epoch ms; the message's own timestamp, not the session's.
    pub created_at: i64,
}

/// The degraded-index report riding on a [`SearchResult`].
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IndexReport {
    #[serde(flatten)]
    pub health: IndexHealth,
    /// Always true — present only when the index IS degraded.
    pub degraded: bool,
    pub repair: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// The query as typed, trimmed.
    pub query: String,
    /// What FTS5 actually ran — differs from `query` only after a rewrite.
    pub effective_query: String,
    /// True when `query` did not parse and was rewritten into quoted phrases.
    pub rewritten: bool,
    /// The session the search was scoped to, or null for the whole forest.
    pub scope: Option<String>,
    pub limit: i64,
    pub count: usize,
    pub hits: Vec<SearchResultHit>,
    /// Present only when this process has swallowed an indexing error, in
    /// which case results may be incomplete and `POST /search/reindex` is the
    /// repair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<IndexReport>,
}

// ---- the safety wrapper -----------------------------------------------------

/// Where a swallowed indexing error is reported. Default logs it.
pub type IndexErrorReporter = Arc<dyn Fn(&BoughError, &Message) + Send + Sync>;

#[derive(Default)]
pub struct SearchSafeOptions {
    /// Where a swallowed indexing error is reported. Absent = `tracing::error!`.
    pub on_error: Option<IndexErrorReporter>,
    /// Injected clock for `lastFailureAt`. Absent = the system clock.
    pub now: Option<Clock>,
}

/// Wrap a `Db` so `index_message` can never fail its caller.
///
/// Every other method delegates untouched, so this is installable at boot over
/// the one real handle and nothing downstream can tell the difference — which
/// is the point: the guarantee has to hold for call sites this module has
/// never heard of. (The TS Proxy + bound-method dance is just a newtype here.)
pub struct SearchSafeDb<D: Db> {
    inner: D,
    health: Mutex<IndexHealth>,
    on_error: IndexErrorReporter,
    now: Clock,
}

impl<D: Db> SearchSafeDb<D> {
    pub fn new(inner: D, opts: SearchSafeOptions) -> Self {
        let on_error: IndexErrorReporter = opts.on_error.unwrap_or_else(|| {
            Arc::new(|err: &BoughError, m: &Message| {
                tracing::error!(
                    "search index write failed for message {} (search results may be \
                     incomplete; POST /search/reindex repairs it): {err}",
                    m.id
                );
            })
        });
        SearchSafeDb {
            inner,
            health: Mutex::new(IndexHealth::default()),
            on_error,
            now: opts.now.unwrap_or_else(bough_core::types::system_clock),
        }
    }
}

impl<D: Db> Db for SearchSafeDb<D> {
    // ---- the one intercepted method ----------------------------------------

    fn index_message(&self, message: &Message) -> Result<(), BoughError> {
        if let Err(err) = self.inner.index_message(message) {
            {
                let mut health = self.health.lock().unwrap();
                health.failures += 1;
                health.last_error = Some(err.to_string());
                health.last_failure_at = Some((self.now)());
            }
            // The report itself must not fail past the write it is protecting.
            let report = self.on_error.clone();
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| report(&err, message)));
        }
        Ok(())
    }

    fn index_health(&self) -> Option<IndexHealth> {
        Some(self.health.lock().unwrap().clone())
    }

    fn heal_search_index(&self) {
        *self.health.lock().unwrap() = IndexHealth::default();
    }

    // ---- everything else delegates untouched --------------------------------

    fn upsert_note(
        &self,
        path: &str,
        title: &str,
        tags: &[String],
        now: i64,
    ) -> Result<i64, BoughError> {
        self.inner.upsert_note(path, title, tags, now)
    }
    fn note_by_path(&self, path: &str) -> Result<Option<NoteRow>, BoughError> {
        self.inner.note_by_path(path)
    }
    fn list_notes(&self) -> Result<Vec<NoteRow>, BoughError> {
        self.inner.list_notes()
    }
    fn notes_for_tags(&self, tags: &[String]) -> Result<Vec<NoteRow>, BoughError> {
        self.inner.notes_for_tags(tags)
    }
    fn set_note_synced(&self, note_id: i64, ts: i64) -> Result<(), BoughError> {
        self.inner.set_note_synced(note_id, ts)
    }
    fn close_note(&self, note_id: i64, at: i64) -> Result<(), BoughError> {
        self.inner.close_note(note_id, at)
    }
    fn put_section(&self, write: &SectionWrite, now: i64) -> Result<i64, BoughError> {
        self.inner.put_section(write, now)
    }
    fn sections_for_note(&self, note_id: i64) -> Result<Vec<SectionRow>, BoughError> {
        self.inner.sections_for_note(note_id)
    }
    fn sections_for_context(
        &self,
        context: &[String],
        exclude_note: Option<i64>,
    ) -> Result<Vec<SectionRow>, BoughError> {
        self.inner.sections_for_context(context, exclude_note)
    }
    fn section_revisions(&self, section_id: i64) -> Result<Vec<SectionRevision>, BoughError> {
        self.inner.section_revisions(section_id)
    }
    fn delete_section(&self, section_id: i64) -> Result<(), BoughError> {
        self.inner.delete_section(section_id)
    }
    fn search_sections(&self, words: &[String], limit: i64) -> Result<Vec<SectionRow>, BoughError> {
        self.inner.search_sections(words, limit)
    }
    fn append_note_log(
        &self,
        note_id: i64,
        ts: i64,
        source: NoteAuthor,
        text: &str,
    ) -> Result<bool, BoughError> {
        self.inner.append_note_log(note_id, ts, source, text)
    }
    fn note_log(&self, note_id: i64, limit: i64) -> Result<Vec<NoteLogRow>, BoughError> {
        self.inner.note_log(note_id, limit)
    }
    fn citation_is_valid(
        &self,
        kind: &str,
        reference: &str,
        tags: &[String],
    ) -> Result<bool, BoughError> {
        self.inner.citation_is_valid(kind, reference, tags)
    }

    fn create_session(&self, session: Session) -> Result<Session, BoughError> {
        self.inner.create_session(session)
    }
    fn get_session(&self, id: &str) -> Result<Option<Session>, BoughError> {
        self.inner.get_session(id)
    }
    fn get_session_runtime(&self, id: &str) -> Result<SessionRuntime, BoughError> {
        self.inner.get_session_runtime(id)
    }
    fn list_sessions(&self) -> Result<Vec<Session>, BoughError> {
        self.inner.list_sessions()
    }
    fn sessions_by_origin(&self, origin_id: &str) -> Result<Vec<Session>, BoughError> {
        self.inner.sessions_by_origin(origin_id)
    }
    fn ancestor_chain(&self, id: &str) -> Result<Vec<Session>, BoughError> {
        self.inner.ancestor_chain(id)
    }
    fn set_session_title(&self, id: &str, title: &str) -> Result<(), BoughError> {
        self.inner.set_session_title(id, title)
    }
    fn set_session_description(&self, id: &str, description: &str) -> Result<(), BoughError> {
        self.inner.set_session_description(id, description)
    }
    fn add_milestone(&self, session_id: &str, ts: i64, text: &str) -> Result<(), BoughError> {
        self.inner.add_milestone(session_id, ts, text)
    }
    fn milestones(
        &self,
        session_id: &str,
    ) -> Result<Vec<bough_core::schema::parts::Milestone>, BoughError> {
        self.inner.milestones(session_id)
    }
    fn set_session_workspace(&self, id: &str, workspace: &str) -> Result<(), BoughError> {
        self.inner.set_session_workspace(id, workspace)
    }
    fn set_session_base(&self, id: &str, base: &str) -> Result<(), BoughError> {
        self.inner.set_session_base(id, base)
    }
    fn set_session_draft(&self, id: &str, draft: Option<&str>) -> Result<(), BoughError> {
        self.inner.set_session_draft(id, draft)
    }
    fn set_session_model(&self, id: &str, model: Option<&str>) -> Result<(), BoughError> {
        self.inner.set_session_model(id, model)
    }
    fn set_session_effort(&self, id: &str, effort: Option<&str>) -> Result<(), BoughError> {
        self.inner.set_session_effort(id, effort)
    }
    fn set_session_outcome(&self, id: &str, ok: bool) -> Result<(), BoughError> {
        self.inner.set_session_outcome(id, ok)
    }
    fn add_session_usage(&self, id: &str, usage: &Usage, at: i64) -> Result<(), BoughError> {
        self.inner.add_session_usage(id, usage, at)
    }
    fn session_usage(&self, id: &str) -> Result<UsageTotals, BoughError> {
        self.inner.session_usage(id)
    }
    fn tree_usage(&self, id: &str) -> Result<UsageTotals, BoughError> {
        self.inner.tree_usage(id)
    }
    fn busy_session_ids(&self) -> Result<HashSet<String>, BoughError> {
        self.inner.busy_session_ids()
    }
    fn create_message(&self, message: Message) -> Result<Message, BoughError> {
        self.inner.create_message(message)
    }
    fn get_message(&self, id: &str) -> Result<Option<Message>, BoughError> {
        self.inner.get_message(id)
    }
    fn messages_for(&self, session_id: &str) -> Result<Vec<Message>, BoughError> {
        self.inner.messages_for(session_id)
    }
    fn thread_for(&self, session_id: &str) -> Result<Vec<Message>, BoughError> {
        self.inner.thread_for(session_id)
    }
    fn update_message(&self, id: &str, parts: &[Part], pending: bool) -> Result<(), BoughError> {
        self.inner.update_message(id, parts, pending)
    }
    fn delete_messages_from(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, BoughError> {
        self.inner.delete_messages_from(session_id, message_id)
    }
    fn create_turn(&self, turn: Turn) -> Result<Turn, BoughError> {
        self.inner.create_turn(turn)
    }
    fn get_turn(&self, id: &str) -> Result<Option<Turn>, BoughError> {
        self.inner.get_turn(id)
    }
    fn turn_for_message(&self, message_id: &str) -> Result<Option<Turn>, BoughError> {
        self.inner.turn_for_message(message_id)
    }
    fn turns_for_session(&self, session_id: &str) -> Result<Vec<Turn>, BoughError> {
        self.inner.turns_for_session(session_id)
    }
    fn turns_by_status(&self, status: TurnStatus) -> Result<Vec<Turn>, BoughError> {
        self.inner.turns_by_status(status)
    }
    fn latest_turn_statuses(&self) -> Result<HashMap<String, TurnStatus>, BoughError> {
        self.inner.latest_turn_statuses()
    }
    fn update_turn(&self, id: &str, patch: TurnPatch) -> Result<(), BoughError> {
        self.inner.update_turn(id, patch)
    }
    fn get_state(&self, root_id: &str, key: &str) -> Result<Option<String>, BoughError> {
        self.inner.get_state(root_id, key)
    }
    fn set_state(&self, root_id: &str, key: &str, value: &str, now: i64) -> Result<(), BoughError> {
        self.inner.set_state(root_id, key, value, now)
    }
    fn list_state(&self, root_id: &str) -> Result<Vec<StateEntry>, BoughError> {
        self.inner.list_state(root_id)
    }
    fn delete_state(&self, root_id: &str, key: &str) -> Result<bool, BoughError> {
        self.inner.delete_state(root_id, key)
    }
    fn create_schedule(&self, schedule: Schedule) -> Result<Schedule, BoughError> {
        self.inner.create_schedule(schedule)
    }
    fn get_schedule(&self, id: &str) -> Result<Option<Schedule>, BoughError> {
        self.inner.get_schedule(id)
    }
    fn list_schedules(&self) -> Result<Vec<Schedule>, BoughError> {
        self.inner.list_schedules()
    }
    fn due_schedules(&self, now: i64) -> Result<Vec<Schedule>, BoughError> {
        self.inner.due_schedules(now)
    }
    fn update_schedule(&self, schedule: &Schedule) -> Result<(), BoughError> {
        self.inner.update_schedule(schedule)
    }
    fn mark_schedule_run(
        &self,
        id: &str,
        last_run_at: i64,
        next_run_at: i64,
    ) -> Result<(), BoughError> {
        self.inner.mark_schedule_run(id, last_run_at, next_run_at)
    }
    fn delete_schedule(&self, id: &str) -> Result<(), BoughError> {
        self.inner.delete_schedule(id)
    }
    fn create_workflow(&self, run: WorkflowRun) -> Result<WorkflowRun, BoughError> {
        self.inner.create_workflow(run)
    }
    fn get_workflow(&self, id: &str) -> Result<Option<WorkflowRun>, BoughError> {
        self.inner.get_workflow(id)
    }
    fn list_workflows(&self, session_id: Option<&str>) -> Result<Vec<WorkflowRun>, BoughError> {
        self.inner.list_workflows(session_id)
    }
    fn unfinished_workflows(&self) -> Result<Vec<WorkflowRun>, BoughError> {
        self.inner.unfinished_workflows()
    }
    fn update_workflow(&self, id: &str, patch: WorkflowPatch) -> Result<(), BoughError> {
        self.inner.update_workflow(id, patch)
    }
    fn create_workflow_agent(&self, agent: WorkflowAgent) -> Result<WorkflowAgent, BoughError> {
        self.inner.create_workflow_agent(agent)
    }
    fn update_workflow_agent(&self, id: &str, patch: WorkflowAgentPatch) -> Result<(), BoughError> {
        self.inner.update_workflow_agent(id, patch)
    }
    fn list_workflow_agents(&self, run_id: &str) -> Result<Vec<WorkflowAgent>, BoughError> {
        self.inner.list_workflow_agents(run_id)
    }
    fn find_workflow_agent(
        &self,
        run_id: &str,
        key: &str,
    ) -> Result<Option<WorkflowAgent>, BoughError> {
        self.inner.find_workflow_agent(run_id, key)
    }
    fn record_command(&self, record: &CommandRecord) -> Result<(), BoughError> {
        self.inner.record_command(record)
    }
    fn command_tag_rows(
        &self,
        repo: &str,
        opts: CommandTagOpts,
    ) -> Result<Vec<CommandTagRow>, BoughError> {
        self.inner.command_tag_rows(repo, opts)
    }
    fn tag_spread(&self, since_ts: Option<i64>) -> Result<(i64, HashMap<String, i64>), BoughError> {
        self.inner.tag_spread(since_ts)
    }
    fn tag_diversity_by_day(
        &self,
        since_ts: i64,
        repo: Option<&str>,
    ) -> Result<Vec<TagDiversityDay>, BoughError> {
        self.inner.tag_diversity_by_day(since_ts, repo)
    }
    fn commands_for_tag(
        &self,
        tag: &str,
        repo: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<TaggedCommand>, BoughError> {
        self.inner.commands_for_tag(tag, repo, limit)
    }
    fn last_for_tags(
        &self,
        tags: &[String],
        repo: Option<&str>,
    ) -> Result<Vec<RecalledCommand>, BoughError> {
        self.inner.last_for_tags(tags, repo)
    }
    fn search_commands(
        &self,
        repo: &str,
        words: &[String],
        limit: i64,
    ) -> Result<Vec<TaggedCommand>, BoughError> {
        self.inner.search_commands(repo, words, limit)
    }
    fn repo_tag_counts(
        &self,
        repo: &str,
        since_ts: i64,
    ) -> Result<HashMap<String, i64>, BoughError> {
        self.inner.repo_tag_counts(repo, since_ts)
    }
    fn prior_failures(
        &self,
        repo: &str,
        cmd: &str,
        since_ts: i64,
        session_id: &str,
    ) -> Result<Option<PriorFailures>, BoughError> {
        self.inner.prior_failures(repo, cmd, since_ts, session_id)
    }
    fn recent_failures(
        &self,
        repo: &str,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<RecentFailure>, BoughError> {
        self.inner.recent_failures(repo, since_ts, limit)
    }
    fn last_success_like(
        &self,
        repo: &str,
        prefix: &str,
        not_cmd: &str,
        since_ts: i64,
    ) -> Result<Option<String>, BoughError> {
        self.inner
            .last_success_like(repo, prefix, not_cmd, since_ts)
    }
    fn program_for_message(&self, message_id: &str) -> Result<Option<String>, BoughError> {
        self.inner.program_for_message(message_id)
    }
    fn search_messages(
        &self,
        query: &str,
        session_id: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<SearchHit>, BoughError> {
        self.inner.search_messages(query, session_id, limit)
    }
    fn rebuild_search_index(&self) -> Result<(), BoughError> {
        self.inner.rebuild_search_index()
    }
    fn close(&self) {
        self.inner.close()
    }
}

// ---- index maintenance ------------------------------------------------------

/// Re-index the messages an orphaned turn left behind, and answer how many.
///
/// A turn that died mid-stream never reached the finish path that indexes its
/// message, so everything the supervisor had already said in it would be
/// unsearchable forever — and boot recovery is the one moment those messages
/// are known, closed and enumerated. Idempotent like every other index write,
/// so a message that *was* indexed simply gets the same rows back.
pub fn index_recovered_messages(db: &dyn Db, message_ids: &[String]) -> usize {
    let mut indexed = 0;
    for message_id in message_ids {
        // Recovery is best-effort and runs before the listener binds: a search
        // index that cannot be written must not stop the server from starting.
        let attempt = (|| -> Result<bool, BoughError> {
            let Some(message) = db.get_message(message_id)? else {
                return Ok(false);
            };
            db.index_message(&message)?;
            Ok(true)
        })();
        match attempt {
            Ok(true) => indexed += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::error!("failed to index recovered message {message_id}: {err}");
            }
        }
    }
    indexed
}

/// What a rebuild covered: the transcript corpus, not FTS internals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RebuildCounts {
    pub messages: usize,
    pub sessions: usize,
}

/// Rebuild the whole index from `messages` and report how many messages it
/// covers.
///
/// The rebuild itself is `db.rebuild_search_index()`, which clears and
/// re-projects through the same function the insert path uses — that shared
/// projection is what makes a rebuild produce results identical to incremental
/// indexing. The count is walked separately because that guarantee is worth
/// more than saving a pass.
///
/// Deliberately unguarded: this is the repair path, asked for explicitly by a
/// human; a rebuild that failed and answered "ok" would leave them believing
/// search had been fixed.
pub fn rebuild_index(db: &dyn Db) -> Result<RebuildCounts, BoughError> {
    if let Err(err) = db.rebuild_search_index() {
        // The one failure worth translating: a rebuild cannot create the table
        // it writes into, so "no such table" needs the restart-then-reindex
        // sentence rather than a 500 that reads as a bug in the rebuild.
        if let Some(cause) = names_the_index(&err) {
            return Err(search_index_unavailable(&cause));
        }
        return Err(err);
    }
    db.heal_search_index();
    let sessions = db.list_sessions()?;
    let mut messages = 0;
    for session in &sessions {
        messages += db.messages_for(&session.id)?.len();
    }
    Ok(RebuildCounts {
        messages,
        sessions: sessions.len(),
    })
}

// ---- query ------------------------------------------------------------------

/// Rewrite a query that FTS5 refused to parse into one it will accept.
///
/// Each whitespace-separated chunk becomes a quoted phrase, joined by AND.
/// That keeps the reading a human intends — every term must appear — while
/// neutralizing the punctuation FTS5 treats as syntax: `what's` becomes the
/// two-token phrase it was indexed as, `foo-bar` matches the hyphenated text,
/// and a stray `"` is escaped by doubling rather than swallowed. Operators are
/// lost, which is correct: this only runs for a query that was not valid
/// operator syntax in the first place.
pub fn quote_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|chunk| format!("\"{}\"", chunk.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// The three SQLite sentences that mean the INDEX failed, not the query.
const INDEX_CAUSES: [&str; 3] = [
    "no such table: messages_fts",
    "no such module: fts5",
    "database disk image is malformed",
];

/// The failure is the index itself rather than the query — with the SQLite
/// cause, in the text's own casing.
///
/// `db.search_messages` renders EVERY error from that statement as "not valid
/// FTS5 syntax", which is right for the case it was written for and wrong for
/// a missing or corrupt `messages_fts`: the user would be told the words they
/// typed are malformed while the real problem is that there is nothing to
/// search. Sniffing the SQLite text is the only discriminator the port
/// exposes, and getting this wrong in either direction only changes which
/// correct-status error is reported, never whether one is.
fn names_the_index(err: &BoughError) -> Option<String> {
    let text = err.to_string();
    let lower = text.to_lowercase();
    for cause in INDEX_CAUSES {
        if let Some(at) = lower.find(cause) {
            return Some(text[at..at + cause.len()].to_string());
        }
    }
    None
}

/// True when the FTS parser, not the corpus, rejected the query.
fn is_syntax_error(err: &BoughError) -> bool {
    matches!(
        err,
        BoughError::Http {
            kind: ErrorKind::BadRequest,
            ..
        }
    ) && names_the_index(err).is_none()
}

/// 503 — the index is gone, not the query. Named separately from a 400 because
/// the fix is different in kind: nothing the user retypes will help, and a
/// rebuild cannot create a table either (the schema is applied at open, and
/// `db/` owns the SQL). Error text is a product surface: this one names what
/// failed, the state that caused it, and the move that resolves it.
fn search_index_unavailable(cause: &str) -> BoughError {
    BoughError::http(
        503,
        ErrorKind::SearchIndexUnavailable,
        format!(
            "the search index is unavailable ({cause}). The transcripts themselves are \
             intact — messages are stored in `messages`, and the index is a projection \
             of them. `messages_fts` is created when the database is opened, so \
             restarting the server recreates it; POST /search/reindex then refills it from \
             the stored messages."
        ),
    )
}

/// Options for [`search_transcripts`].
#[derive(Clone, Debug, Default)]
pub struct SearchOpts {
    pub session_id: Option<String>,
    pub limit: Option<i64>,
}

/// Run one search. The pure-ish core the route is a wrapper over: it takes a
/// `Db` and returns data, so it is testable without a request.
pub fn search_transcripts(
    db: &dyn Db,
    query: &str,
    opts: SearchOpts,
) -> Result<SearchResult, BoughError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(BoughError::bad_request(NEEDS_A_QUERY));
    }
    let limit = opts.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    if let Some(session_id) = opts.session_id.as_deref() {
        if db.get_session(session_id)?.is_none() {
            return Err(BoughError::not_found(format!(
                "no session {session_id} — drop ?sessionId= to search every transcript."
            )));
        }
    }

    let mut effective_query = trimmed.to_string();
    let mut rewritten = false;
    let raw: Vec<SearchHit> =
        match db.search_messages(trimmed, opts.session_id.as_deref(), Some(limit)) {
            Ok(hits) => hits,
            Err(err) => {
                // A parse failure is retried as quoted phrases; a broken index
                // is reported as itself; anything else is the database talking
                // and belongs to the caller unchanged.
                if let Some(cause) = names_the_index(&err) {
                    return Err(search_index_unavailable(&cause));
                }
                if !is_syntax_error(&err) {
                    return Err(err);
                }
                effective_query = quote_query(trimmed);
                rewritten = true;
                if effective_query.is_empty() {
                    return Err(err);
                }
                db.search_messages(&effective_query, opts.session_id.as_deref(), Some(limit))?
            }
        };

    let mut sessions: HashMap<String, Option<Session>> = HashMap::new();
    let mut hits: Vec<SearchResultHit> = Vec::new();
    for hit in raw {
        // A hit whose message is gone is index drift, not a result: showing a
        // snippet with nothing to open would be worse than showing one fewer
        // hit.
        let Some(message) = db.get_message(&hit.message_id)? else {
            continue;
        };
        let session = match sessions.entry(hit.session_id.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(db.get_session(&hit.session_id)?)
            }
        };
        hits.push(SearchResultHit {
            message_id: hit.message_id,
            session_id: hit.session_id,
            title: session
                .as_ref()
                .map(|s| s.title.clone())
                .unwrap_or_else(|| "(unknown session)".to_string()),
            kind: session
                .as_ref()
                .map(|s| s.kind)
                .unwrap_or(SessionKind::Root),
            collapsed: session
                .as_ref()
                .map(|s| is_collapsed_kind(s.kind))
                .unwrap_or(false),
            origin_id: session.as_ref().and_then(|s| s.origin_id.clone()),
            role: message.role,
            snippet: hit.snippet,
            created_at: hit.created_at,
        });
    }

    let health = db.index_health();
    let index = match health {
        Some(health) if health.failures > 0 => Some(IndexReport {
            health,
            degraded: true,
            repair: "POST /search/reindex rebuilds the index from the stored messages.".to_string(),
        }),
        _ => None,
    };
    Ok(SearchResult {
        query: trimmed.to_string(),
        effective_query,
        rewritten,
        scope: opts.session_id,
        limit,
        count: hits.len(),
        hits,
        index,
    })
}

// ---- routes -----------------------------------------------------------------

/// Percent-decode one query-string value (`+` = space, `%XX` = byte).
fn decode_component(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let decoded = v
                    .get(i + 1..i + 3)
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(b) = decoded {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first occurrence of `name` in the raw query string, decoded — the
/// `URL.searchParams.get` subset this route needs.
fn query_param(raw_query: &str, name: &str) -> Option<String> {
    raw_query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        if decode_component(k) == name {
            Some(decode_component(v))
        } else {
            None
        }
    })
}

/// `GET /search?q=…&sessionId=…&limit=…` — keyword search over every
/// transcript.
pub fn search() -> Handler {
    handler(|req, ctx, _params| async move {
        let raw_query = req.uri().query().unwrap_or("").to_string();
        let q = query_param(&raw_query, "q").unwrap_or_default();
        // Answered before the schema, because the schema's version of "you
        // typed nothing" is an issue list and this is the single most likely
        // way to arrive here.
        if q.trim().is_empty() {
            return Err(BoughError::bad_request(NEEDS_A_QUERY));
        }
        let session_id = query_param(&raw_query, "sessionId").filter(|s| !s.is_empty());
        // Query strings are strings: the numeric limit is coerced first, and a
        // value that is not a whole number in range is named, not dumped.
        let raw_limit = query_param(&raw_query, "limit");
        let limit: Option<u32> = match raw_limit.as_deref() {
            None => None,
            Some(raw) => match raw.parse::<f64>() {
                Ok(n) if n.fract() == 0.0 && (1.0..=MAX_LIMIT as f64).contains(&n) => {
                    Some(n as u32)
                }
                _ => {
                    return Err(BoughError::bad_request(format!(
                        "invalid search (limit: must be an integer between 1 and {MAX_LIMIT}) \
                         — GET /search?q=<words>[&sessionId=<id>][&limit=1..{MAX_LIMIT}]"
                    )));
                }
            },
        };
        let parsed = SearchQuery {
            q,
            session_id,
            limit,
        };
        parsed.validate().map_err(|err| {
            BoughError::bad_request(format!(
                "invalid search ({err}) — GET /search?q=<words>[&sessionId=<id>]\
                 [&limit=1..{MAX_LIMIT}]"
            ))
        })?;

        let db = ctx.db.lock().unwrap();
        let result = search_transcripts(
            &*db,
            &parsed.q,
            SearchOpts {
                session_id: parsed.session_id,
                limit: parsed.limit.map(i64::from),
            },
        )?;
        Ok(json(&result, 200))
    })
}

/// `POST /search/reindex` — rebuild the index from the stored messages.
///
/// The repair for the drift a swallowed indexing error leaves behind, and the
/// reason swallowing one is defensible at all. `messages` is the transcript
/// corpus, not a count of index rows: a message with no prose contributes
/// nothing to index, so the two are not the same number and reporting FTS
/// internals here would invite reading a legitimate difference as a bug.
pub fn reindex() -> Handler {
    handler(|_req, ctx, _params| async move {
        let counts = {
            let db = ctx.db.lock().unwrap();
            rebuild_index(&*db)?
        };
        Ok(json(
            &serde_json::json!({
                "rebuilt": true,
                "messages": counts.messages,
                "sessions": counts.sessions,
            }),
            200,
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};

    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions};
    use crate::http::testutil;
    use bough_core::bus::Bus;
    use bough_core::db::sqlite_db::{DbOptions, SqliteDb};
    use bough_core::turn::queue::TurnRegistry;
    use bough_core::types::{system_clock, AppCtx, HostState, SharedDb, TurnStarter};

    // ---- fixtures -----------------------------------------------------------

    struct Fx {
        ctx: AppCtx,
        /// How many indexing errors the wrapper swallowed (reported to us).
        swallowed: Arc<Mutex<usize>>,
    }

    struct NoopStarter;
    impl TurnStarter for NoopStarter {
        fn start_turn(&self, _ctx: &AppCtx, _s: &bough_core::schema::parts::Session, _m: &Message) {
        }
    }

    /// A ctx whose `db` is wrapped exactly as boot wraps it, so every test
    /// here exercises the handle the server actually serves with. `path` opts
    /// into a real file for the tests that need a second connection.
    fn fixture(path: Option<&str>) -> Fx {
        let raw = SqliteDb::new(path.unwrap_or(":memory:"), DbOptions::default()).unwrap();
        let swallowed = Arc::new(Mutex::new(0));
        let counter = swallowed.clone();
        let wrapped = SearchSafeDb::new(
            raw,
            SearchSafeOptions {
                on_error: Some(Arc::new(move |_e, _m| {
                    *counter.lock().unwrap() += 1;
                })),
                now: None,
            },
        );
        let db: SharedDb = Arc::new(Mutex::new(wrapped));
        let ctx = AppCtx {
            db,
            bus: Arc::new(Bus::new(system_clock())),
            llm: None,
            model: Some("test-model".into()),
            effort: None,
            now: system_clock(),
            cheap: None,
            host: Arc::new(HostState::new()),
            starter: Arc::new(RwLock::new(Some(Arc::new(NoopStarter)))),
            turn_registry: Arc::new(TurnRegistry::new()),
            model_defaults_path: Some(
                std::env::temp_dir()
                    .join(format!("bough-test-{}", uuid::Uuid::new_v4()))
                    .join("model.json"),
            ),
        };
        Fx { ctx, swallowed }
    }

    static CLOCK: std::sync::atomic::AtomicI64 =
        std::sync::atomic::AtomicI64::new(1_700_000_000_000);
    fn tick() -> i64 {
        CLOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// A session row, written straight through the port — no HTTP needed to
    /// search.
    fn session(fx: &Fx, title: &str, kind: SessionKind, origin_id: Option<&str>) -> Session {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: title.into(),
                kind,
                created_at: tick(),
                parent_id: None,
                origin_id: origin_id.map(str::to_string),
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
                description: None,
            })
            .unwrap()
    }

    /// A message plus its index write, exactly as every insert path does it.
    fn say(fx: &Fx, s: &Session, text: &str, role: Role) -> Message {
        let db = fx.ctx.db.lock().unwrap();
        let stored = db
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: s.id.clone(),
                role,
                parts: vec![Part::Text { text: text.into() }],
                pending: false,
                created_at: tick(),
            })
            .unwrap();
        db.index_message(&stored).unwrap();
        stored
    }

    async fn search_ok(fx: &Fx, query: &str) -> serde_json::Value {
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call.call(testutil::get(&format!("/search?{query}"))).await;
        assert_eq!(res.status(), 200);
        testutil::body_json(res).await
    }

    fn hit_ids(result: &serde_json::Value) -> Vec<String> {
        result["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["messageId"].as_str().unwrap().to_string())
            .collect()
    }

    // ---- querying -----------------------------------------------------------

    #[tokio::test]
    async fn a_multi_word_query_is_an_implicit_and_not_a_phrase_and_not_an_or() {
        let fx = fixture(None);
        let s = session(&fx, "index work", SessionKind::Root, None);
        let both = say(
            &fx,
            &s,
            "the patch engine rejects an overlapping conflict range",
            Role::User,
        );
        let adjacent = say(&fx, &s, "a patch conflict names the file", Role::User);
        say(&fx, &s, "patch applies cleanly here", Role::User);
        say(&fx, &s, "a conflict in the schedule spec", Role::User);

        let result = search_ok(&fx, "q=patch+conflict").await;
        let mut ids = hit_ids(&result);
        ids.sort();
        let mut want = vec![both.id, adjacent.id];
        want.sort();
        assert_eq!(ids, want, "both terms required, order-free");
        assert_eq!(result["rewritten"], false);
        assert_eq!(result["effectiveQuery"], "patch conflict");
        assert_eq!(result["count"], 2);
        assert_eq!(result["limit"], DEFAULT_LIMIT);
        assert_eq!(result["scope"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_quoted_phrase_requires_adjacency_the_same_words_apart_do_not_match() {
        let fx = fixture(None);
        let s = session(&fx, "phrases", SessionKind::Root, None);
        let adjacent = say(&fx, &s, "we hit a patch conflict on files.ts", Role::User);
        say(
            &fx,
            &s,
            "the patch applied but the conflict was elsewhere",
            Role::User,
        );

        let phrase = search_ok(&fx, "q=%22patch%20conflict%22").await;
        assert_eq!(hit_ids(&phrase), vec![adjacent.id]);
        assert_eq!(phrase["rewritten"], false);

        // The same two words unquoted find both — the phrase is what narrowed
        // it.
        let loose = search_ok(&fx, "q=patch+conflict").await;
        assert_eq!(loose["count"], 2);
    }

    #[tokio::test]
    async fn ranking_puts_the_denser_match_first() {
        let fx = fixture(None);
        let s = session(&fx, "ranking", SessionKind::Root, None);
        // Written in the losing order on purpose: an implementation that
        // returned insertion order rather than rank would pass a test seeded
        // the other way round.
        let sparse = say(
            &fx,
            &s,
            "a long note about the schedule ticker, the queue drain, the artifact store, \
             the comment sidecar and the changes rail, which mentions patch exactly once \
             and otherwise talks about entirely unrelated machinery for several lines",
            Role::User,
        );
        let dense = say(
            &fx,
            &s,
            "patch, patch, patch — the patch grammar",
            Role::User,
        );

        let result = search_ok(&fx, "q=patch").await;
        assert_eq!(
            hit_ids(&result),
            vec![dense.id, sparse.id],
            "bm25 ranks the short, term-dense message above the long one-mention message"
        );
    }

    #[tokio::test]
    async fn a_hit_carries_the_session_its_title_and_kind_the_role_and_the_timestamp() {
        let fx = fixture(None);
        let root = session(&fx, "the spawner", SessionKind::Root, None);
        let child = session(
            &fx,
            "review files.ts",
            SessionKind::Subagent,
            Some(&root.id),
        );
        let m = say(
            &fx,
            &child,
            "reticulating splines in the delegated branch",
            Role::Supervisor,
        );

        let result = search_ok(&fx, "q=reticulating").await;
        assert_eq!(result["count"], 1);
        let hit = &result["hits"][0];
        assert_eq!(hit["messageId"], m.id.as_str());
        assert_eq!(hit["sessionId"], child.id.as_str());
        assert_eq!(hit["title"], "review files.ts");
        assert_eq!(hit["kind"], "subagent");
        assert_eq!(hit["collapsed"], true, "a subagent opens only on drill-in");
        assert_eq!(hit["originId"], root.id.as_str());
        assert_eq!(hit["role"], "supervisor");
        assert_eq!(hit["createdAt"], m.created_at);
        assert!(hit["snippet"].as_str().unwrap().contains("reticulating"));
    }

    #[tokio::test]
    async fn session_id_scopes_the_search_an_unknown_one_is_a_404_not_an_empty_answer() {
        let fx = fixture(None);
        let a = session(&fx, "session a", SessionKind::Root, None);
        let b = session(&fx, "session b", SessionKind::Root, None);
        let in_a = say(&fx, &a, "splines everywhere", Role::User);
        say(&fx, &b, "splines here too", Role::User);

        assert_eq!(search_ok(&fx, "q=splines").await["count"], 2);
        let scoped = search_ok(&fx, &format!("q=splines&sessionId={}", a.id)).await;
        assert_eq!(hit_ids(&scoped), vec![in_a.id]);
        assert_eq!(scoped["scope"], a.id.as_str());

        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let missing = call
            .call(testutil::get("/search?q=splines&sessionId=nope"))
            .await;
        assert_eq!(missing.status(), 404);
        let body = testutil::body_json(missing).await;
        assert!(
            body["error"].as_str().unwrap().contains("no session nope"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn limit_is_validated_and_honored_an_empty_query_is_a_400_that_says_what_to_type() {
        let fx = fixture(None);
        let s = session(&fx, "many", SessionKind::Root, None);
        for i in 0..5 {
            say(&fx, &s, &format!("splines number {i}"), Role::User);
        }

        assert_eq!(search_ok(&fx, "q=splines&limit=2").await["count"], 2);

        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let empty = call.call(testutil::get("/search?q=%20%20")).await;
        assert_eq!(empty.status(), 400);
        let body = testutil::body_json(empty).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("search needs a query"),
            "{body}"
        );

        let bad = call.call(testutil::get("/search?q=splines&limit=0")).await;
        assert_eq!(
            bad.status(),
            400,
            "limit is validated at the boundary, not silently fixed"
        );
        let why = testutil::body_json(bad).await;
        let why = why["error"].as_str().unwrap();
        assert!(
            why.contains("invalid search (limit: "),
            "the issue is named: {why}"
        );
        assert!(!why.contains("\"code\""), "{why}");

        // Absent entirely, which is how most people arrive here.
        let none = call.call(testutil::get("/search")).await;
        assert_eq!(none.status(), 400);
        let body = testutil::body_json(none).await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("search needs a query"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_query_fts5_cannot_parse_is_rewritten_into_phrases_and_the_rewrite_is_reported() {
        let fx = fixture(None);
        let s = session(&fx, "punctuation", SessionKind::Root, None);
        let m = say(&fx, &s, "what's up with the foo-bar helper", Role::User);
        say(&fx, &s, "nothing to do with either word", Role::User);

        // Bare `what's` and `foo-bar` are FTS5 syntax errors, not zero-result
        // searches.
        let result = search_ok(&fx, "q=what%27s%20foo-bar").await;
        assert_eq!(result["rewritten"], true);
        assert_eq!(result["effectiveQuery"], "\"what's\" AND \"foo-bar\"");
        assert_eq!(hit_ids(&result), vec![m.id]);

        // A valid operator query is never rewritten — the fallback only runs
        // on a parse failure, so `OR` keeps meaning OR.
        let operators = search_ok(&fx, "q=nothing+OR+helper").await;
        assert_eq!(operators["rewritten"], false);
        assert_eq!(operators["count"], 2);
    }

    #[test]
    fn quote_query_escapes_an_embedded_quote_instead_of_producing_invalid_syntax() {
        assert_eq!(quote_query("a b"), "\"a\" AND \"b\"");
        assert_eq!(quote_query("say \"hi\""), "\"say\" AND \"\"\"hi\"\"\"");
        assert_eq!(quote_query("   "), "");
    }

    // ---- the index is never load-bearing ------------------------------------

    /// Break the FTS table for real, through a second connection to the same
    /// file.
    ///
    /// A stubbed `index_message` would prove only that the wrapper catches
    /// what a stub throws. This produces the actual failure — `no such table:
    /// messages_fts` from inside the live handle's own statement — which is
    /// what a corrupted or half-created index looks like in production.
    fn break_fts(path: &str) {
        let side = rusqlite::Connection::open(path).unwrap();
        side.execute_batch("DROP TABLE messages_fts").unwrap();
    }

    fn temp_db() -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("bough-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bough.db");
        (dir.clone(), path.to_string_lossy().into_owned())
    }

    #[tokio::test]
    async fn an_fts_write_failure_does_not_break_message_insertion() {
        let (dir, path) = temp_db();
        let fx = fixture(Some(&path));
        let s = session(&fx, "a real session", SessionKind::Root, None);
        // Healthy first, so the failure below is the only difference between
        // the two.
        say(&fx, &s, "before the index broke", Role::User);
        assert_eq!(
            fx.ctx
                .db
                .lock()
                .unwrap()
                .search_messages("broke", None, None)
                .unwrap()
                .len(),
            1
        );

        break_fts(&path);

        // The whole point: the HTTP write path still lands the message.
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let res = call
            .call(testutil::req(
                "POST",
                &format!("/sessions/{}/messages", s.id),
                Some(serde_json::json!({"text": "after the index broke"})),
            ))
            .await;
        assert_eq!(res.status(), 202);
        let messages = fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap();
        assert_eq!(
            messages.len(),
            2,
            "the message is persisted despite the index failure"
        );

        // And direct inserts too — the guarantee is about `index_message`, not
        // about HTTP.
        say(&fx, &s, "and again", Role::User);
        assert_eq!(
            fx.ctx.db.lock().unwrap().messages_for(&s.id).unwrap().len(),
            3
        );

        // It failed quietly, but not invisibly: the failure is counted and
        // reported.
        assert_eq!(*fx.swallowed.lock().unwrap(), 2);
        let health = fx.ctx.db.lock().unwrap().index_health().unwrap();
        assert_eq!(health.failures, 2);
        assert!(health.last_error.unwrap().contains("messages_fts"));
        assert!(health.last_failure_at.unwrap() > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_missing_index_is_a_503_about_the_index_never_a_400_about_the_query() {
        let (dir, path) = temp_db();
        let fx = fixture(Some(&path));
        let s = session(&fx, "degraded", SessionKind::Root, None);
        say(&fx, &s, "indexed while healthy", Role::User);
        break_fts(&path);
        say(
            &fx,
            &s,
            "written while broken and therefore unfindable",
            Role::User,
        );

        // `db.search_messages` renders every failure of that statement as
        // "not valid FTS5 syntax", so without the translation the user is
        // told their word is malformed while the real problem is that there
        // is nothing to search.
        let call = create_handler(fx.ctx.clone(), CreateHandlerOptions::default());
        let during = call.call(testutil::get("/search?q=indexed")).await;
        assert_eq!(during.status(), 503);
        let body = testutil::body_json(during).await;
        let error = body["error"].as_str().unwrap();
        assert!(
            error.contains("search index is unavailable (no such table: messages_fts)"),
            "{error}"
        );
        assert!(
            error.contains("restarting the server recreates it"),
            "{error}"
        );
        assert!(!error.contains("not valid FTS5 syntax"), "{error}");

        // And a rebuild says the same thing rather than 500ing: it cannot
        // create a table.
        let cannot = call
            .call(testutil::req("POST", "/search/reindex", None))
            .await;
        assert_eq!(cannot.status(), 503);

        // Restarting is what recreates it (the schema is applied at open).
        // Same file, fresh handle — exactly what the error tells the user to
        // do.
        fx.ctx.db.lock().unwrap().close();
        drop(fx);
        let restarted = fixture(Some(&path));
        let call = create_handler(restarted.ctx.clone(), CreateHandlerOptions::default());
        let before = search_ok(&restarted, "q=unfindable").await;
        assert_eq!(
            before["count"], 0,
            "the index is empty until it is refilled"
        );

        let repaired = call
            .call(testutil::req("POST", "/search/reindex", None))
            .await;
        assert_eq!(repaired.status(), 200);
        assert_eq!(
            testutil::body_json(repaired).await,
            serde_json::json!({"rebuilt": true, "messages": 2, "sessions": 1})
        );

        let after = search_ok(&restarted, "q=unfindable").await;
        assert_eq!(
            after["count"], 1,
            "the message written while broken is searchable again"
        );
        assert!(
            after.get("index").is_none(),
            "and the process starts with a clean counter: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_degraded_index_is_reported_on_every_search_until_a_rebuild_repairs_it() {
        let (dir, path) = temp_db();
        let fx = fixture(Some(&path));
        let s = session(&fx, "degraded", SessionKind::Root, None);
        say(&fx, &s, "indexed while healthy", Role::User);

        // A write that fails while the index is broken, swallowed by the
        // wrapper on the insert path.
        break_fts(&path);
        say(&fx, &s, "written while the index was locked", Role::User);

        // Repair the substrate the way a rebuild CAN repair it — recreate the
        // table (same DDL as `db/schema.sql`) so only the missing rows remain
        // wrong. The counter must keep reporting until the rebuild runs.
        {
            let side = rusqlite::Connection::open(&path).unwrap();
            side.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                   text, message_id UNINDEXED, session_id UNINDEXED,
                   tokenize = 'unicode61 remove_diacritics 2'
                 )",
            )
            .unwrap();
        }

        let during = {
            let db = fx.ctx.db.lock().unwrap();
            search_transcripts(&*db, "indexed", SearchOpts::default()).unwrap()
        };
        assert_eq!(
            during.count, 0,
            "the incremental rows were lost with the dropped table"
        );
        let index = during.index.expect("degraded report present");
        assert!(index.degraded);
        assert_eq!(index.health.failures, 1);
        assert!(index.health.last_error.unwrap().contains("messages_fts"));
        assert!(index.repair.contains("reindex"));
        {
            let db = fx.ctx.db.lock().unwrap();
            let locked = search_transcripts(&*db, "locked", SearchOpts::default()).unwrap();
            assert_eq!(
                locked.count, 0,
                "the swallowed write is exactly the missing result the report warns about"
            );
        }

        {
            let db = fx.ctx.db.lock().unwrap();
            rebuild_index(&*db).unwrap();
            let after = search_transcripts(&*db, "locked", SearchOpts::default()).unwrap();
            assert!(
                after.index.is_none(),
                "the counter is cleared by the repair"
            );
            assert_eq!(
                after.count, 1,
                "and the missing message is searchable again"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wrapper_delegates_every_other_method_to_the_real_handle() {
        let fx = fixture(None);
        let db = fx.ctx.db.lock().unwrap();
        let root = db
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: "root".into(),
                kind: SessionKind::Root,
                created_at: tick(),
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
                description: None,
            })
            .unwrap();
        let child = db
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: "child".into(),
                kind: SessionKind::Fork,
                created_at: tick(),
                parent_id: Some(root.id.clone()),
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
                description: None,
            })
            .unwrap();
        for (s, text) in [(&root, "ancestor prose"), (&child, "own prose")] {
            let m = db
                .create_message(Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    session_id: s.id.clone(),
                    role: Role::User,
                    parts: vec![Part::Text {
                        text: text.to_string(),
                    }],
                    pending: false,
                    created_at: tick(),
                })
                .unwrap();
            db.index_message(&m).unwrap();
        }

        assert_eq!(db.get_session(&root.id).unwrap().unwrap().title, "root");
        assert_eq!(db.thread_for(&child.id).unwrap().len(), 2);
        assert_eq!(
            db.ancestor_chain(&child.id)
                .unwrap()
                .iter()
                .map(|s| &s.id)
                .collect::<Vec<_>>(),
            vec![&root.id, &child.id]
        );
        assert_eq!(db.list_sessions().unwrap().len(), 2);
        assert_eq!(db.busy_session_ids().unwrap().len(), 0);
        db.set_session_title(&root.id, "renamed").unwrap();
        assert_eq!(db.get_session(&root.id).unwrap().unwrap().title, "renamed");
        assert_eq!(db.search_messages("prose", None, None).unwrap().len(), 2);
    }

    // ---- rebuild and recovery ----------------------------------------------

    #[test]
    fn a_rebuild_produces_exactly_what_incremental_indexing_produced() {
        let fx = fixture(None);
        let a = session(&fx, "a", SessionKind::Root, None);
        let b = session(&fx, "b", SessionKind::Root, None);
        say(
            &fx,
            &a,
            "the patch grammar and its conflict rules",
            Role::User,
        );
        say(&fx, &b, "another patch, another conflict", Role::User);
        say(
            &fx,
            &b,
            "no prose here indexes nothing relevant",
            Role::User,
        );

        let db = fx.ctx.db.lock().unwrap();
        let incremental = db.search_messages("patch", None, None).unwrap();
        let counts = rebuild_index(&*db).unwrap();
        assert_eq!(
            counts,
            RebuildCounts {
                messages: 3,
                sessions: 2
            }
        );
        assert_eq!(
            db.search_messages("patch", None, None).unwrap(),
            incremental,
            "rebuild == incremental"
        );
    }

    #[test]
    fn recovered_turn_messages_are_indexed_at_boot_and_a_missing_one_is_skipped() {
        let fx = fixture(None);
        let s = session(&fx, "crashed mid-turn", SessionKind::Root, None);
        let db = fx.ctx.db.lock().unwrap();
        // Exactly what a died-mid-stream turn leaves: a message persisted by
        // the runner that never reached the finish path where indexing
        // happens.
        let stranded = db
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: s.id.clone(),
                role: Role::Supervisor,
                parts: vec![Part::Text {
                    text: "half a sentence about the orphaned turn".into(),
                }],
                pending: false,
                created_at: tick(),
            })
            .unwrap();
        assert!(
            db.search_messages("orphaned", None, None)
                .unwrap()
                .is_empty(),
            "unindexed to begin with"
        );

        let indexed = index_recovered_messages(
            &*db,
            &[
                stranded.id.clone(),
                "a message the recovery pass named but the database does not have".to_string(),
            ],
        );
        assert_eq!(indexed, 1);
        assert_eq!(
            db.search_messages("orphaned", None, None)
                .unwrap()
                .iter()
                .map(|h| &h.message_id)
                .collect::<Vec<_>>(),
            vec![&stranded.id]
        );

        // Idempotent: running it twice does not double the rows.
        index_recovered_messages(&*db, std::slice::from_ref(&stranded.id));
        assert_eq!(db.search_messages("orphaned", None, None).unwrap().len(), 1);
    }

    /// A handle with index drift: `search_messages` answers a hit whose
    /// message `get_message` cannot fetch. The real statements cannot produce
    /// this (the search JOINs `messages`), so the drift is injected at the
    /// port, exactly as the TS test proxies `getMessage` — the skip is
    /// defensive, and this is the only way to reach it.
    struct DriftDb<D: Db>(D);

    impl<D: Db> Db for DriftDb<D> {
        fn get_message(&self, _id: &str) -> Result<Option<Message>, BoughError> {
            Ok(None)
        }
        fn get_session(&self, id: &str) -> Result<Option<Session>, BoughError> {
            self.0.get_session(id)
        }
        fn search_messages(
            &self,
            query: &str,
            session_id: Option<&str>,
            limit: Option<i64>,
        ) -> Result<Vec<SearchHit>, BoughError> {
            self.0.search_messages(query, session_id, limit)
        }
        // Nothing else participates in `search_transcripts`.
        fn upsert_note(&self, _: &str, _: &str, _: &[String], _: i64) -> Result<i64, BoughError> {
            unreachable!()
        }
        fn note_by_path(&self, _: &str) -> Result<Option<NoteRow>, BoughError> {
            unreachable!()
        }
        fn list_notes(&self) -> Result<Vec<NoteRow>, BoughError> {
            unreachable!()
        }
        fn notes_for_tags(&self, _: &[String]) -> Result<Vec<NoteRow>, BoughError> {
            unreachable!()
        }
        fn set_note_synced(&self, _: i64, _: i64) -> Result<(), BoughError> {
            unreachable!()
        }
        fn close_note(&self, _: i64, _: i64) -> Result<(), BoughError> {
            unreachable!()
        }
        fn put_section(&self, _: &SectionWrite, _: i64) -> Result<i64, BoughError> {
            unreachable!()
        }
        fn sections_for_note(&self, _: i64) -> Result<Vec<SectionRow>, BoughError> {
            unreachable!()
        }
        fn sections_for_context(
            &self,
            _: &[String],
            _: Option<i64>,
        ) -> Result<Vec<SectionRow>, BoughError> {
            unreachable!()
        }
        fn section_revisions(&self, _: i64) -> Result<Vec<SectionRevision>, BoughError> {
            unreachable!()
        }
        fn delete_section(&self, _: i64) -> Result<(), BoughError> {
            unreachable!()
        }
        fn search_sections(&self, _: &[String], _: i64) -> Result<Vec<SectionRow>, BoughError> {
            unreachable!()
        }
        fn append_note_log(
            &self,
            _: i64,
            _: i64,
            _: NoteAuthor,
            _: &str,
        ) -> Result<bool, BoughError> {
            unreachable!()
        }
        fn note_log(&self, _: i64, _: i64) -> Result<Vec<NoteLogRow>, BoughError> {
            unreachable!()
        }
        fn citation_is_valid(&self, _: &str, _: &str, _: &[String]) -> Result<bool, BoughError> {
            unreachable!()
        }
        fn create_session(&self, _: Session) -> Result<Session, BoughError> {
            unreachable!()
        }
        fn get_session_runtime(&self, _: &str) -> Result<SessionRuntime, BoughError> {
            unreachable!()
        }
        fn list_sessions(&self) -> Result<Vec<Session>, BoughError> {
            unreachable!()
        }
        fn sessions_by_origin(&self, _: &str) -> Result<Vec<Session>, BoughError> {
            unreachable!()
        }
        fn ancestor_chain(&self, _: &str) -> Result<Vec<Session>, BoughError> {
            unreachable!()
        }
        fn set_session_title(&self, _: &str, _: &str) -> Result<(), BoughError> {
            unreachable!()
        }
        fn set_session_description(&self, _: &str, _: &str) -> Result<(), BoughError> {
            unreachable!()
        }
        fn add_milestone(&self, _: &str, _: i64, _: &str) -> Result<(), BoughError> {
            unreachable!()
        }
        fn milestones(
            &self,
            _: &str,
        ) -> Result<Vec<bough_core::schema::parts::Milestone>, BoughError> {
            unreachable!()
        }
        fn set_session_workspace(&self, _: &str, _: &str) -> Result<(), BoughError> {
            unreachable!()
        }
        fn set_session_base(&self, _: &str, _: &str) -> Result<(), BoughError> {
            unreachable!()
        }
        fn set_session_draft(&self, _: &str, _: Option<&str>) -> Result<(), BoughError> {
            unreachable!()
        }
        fn set_session_model(&self, _: &str, _: Option<&str>) -> Result<(), BoughError> {
            unreachable!()
        }
        fn set_session_effort(&self, _: &str, _: Option<&str>) -> Result<(), BoughError> {
            unreachable!()
        }
        fn set_session_outcome(&self, _: &str, _: bool) -> Result<(), BoughError> {
            unreachable!()
        }
        fn add_session_usage(&self, _: &str, _: &Usage, _: i64) -> Result<(), BoughError> {
            unreachable!()
        }
        fn session_usage(&self, _: &str) -> Result<UsageTotals, BoughError> {
            unreachable!()
        }
        fn tree_usage(&self, _: &str) -> Result<UsageTotals, BoughError> {
            unreachable!()
        }
        fn busy_session_ids(&self) -> Result<HashSet<String>, BoughError> {
            unreachable!()
        }
        fn create_message(&self, _: Message) -> Result<Message, BoughError> {
            unreachable!()
        }
        fn messages_for(&self, _: &str) -> Result<Vec<Message>, BoughError> {
            unreachable!()
        }
        fn thread_for(&self, _: &str) -> Result<Vec<Message>, BoughError> {
            unreachable!()
        }
        fn update_message(&self, _: &str, _: &[Part], _: bool) -> Result<(), BoughError> {
            unreachable!()
        }
        fn delete_messages_from(&self, _: &str, _: &str) -> Result<Vec<String>, BoughError> {
            unreachable!()
        }
        fn create_turn(&self, _: Turn) -> Result<Turn, BoughError> {
            unreachable!()
        }
        fn get_turn(&self, _: &str) -> Result<Option<Turn>, BoughError> {
            unreachable!()
        }
        fn turn_for_message(&self, _: &str) -> Result<Option<Turn>, BoughError> {
            unreachable!()
        }
        fn turns_for_session(&self, _: &str) -> Result<Vec<Turn>, BoughError> {
            unreachable!()
        }
        fn turns_by_status(&self, _: TurnStatus) -> Result<Vec<Turn>, BoughError> {
            unreachable!()
        }
        fn latest_turn_statuses(&self) -> Result<HashMap<String, TurnStatus>, BoughError> {
            unreachable!()
        }
        fn update_turn(&self, _: &str, _: TurnPatch) -> Result<(), BoughError> {
            unreachable!()
        }
        fn get_state(&self, _: &str, _: &str) -> Result<Option<String>, BoughError> {
            unreachable!()
        }
        fn set_state(&self, _: &str, _: &str, _: &str, _: i64) -> Result<(), BoughError> {
            unreachable!()
        }
        fn list_state(&self, _: &str) -> Result<Vec<StateEntry>, BoughError> {
            unreachable!()
        }
        fn delete_state(&self, _: &str, _: &str) -> Result<bool, BoughError> {
            unreachable!()
        }
        fn create_schedule(&self, _: Schedule) -> Result<Schedule, BoughError> {
            unreachable!()
        }
        fn get_schedule(&self, _: &str) -> Result<Option<Schedule>, BoughError> {
            unreachable!()
        }
        fn list_schedules(&self) -> Result<Vec<Schedule>, BoughError> {
            unreachable!()
        }
        fn due_schedules(&self, _: i64) -> Result<Vec<Schedule>, BoughError> {
            unreachable!()
        }
        fn update_schedule(&self, _: &Schedule) -> Result<(), BoughError> {
            unreachable!()
        }
        fn mark_schedule_run(&self, _: &str, _: i64, _: i64) -> Result<(), BoughError> {
            unreachable!()
        }
        fn delete_schedule(&self, _: &str) -> Result<(), BoughError> {
            unreachable!()
        }
        fn create_workflow(&self, _: WorkflowRun) -> Result<WorkflowRun, BoughError> {
            unreachable!()
        }
        fn get_workflow(&self, _: &str) -> Result<Option<WorkflowRun>, BoughError> {
            unreachable!()
        }
        fn list_workflows(&self, _: Option<&str>) -> Result<Vec<WorkflowRun>, BoughError> {
            unreachable!()
        }
        fn unfinished_workflows(&self) -> Result<Vec<WorkflowRun>, BoughError> {
            unreachable!()
        }
        fn update_workflow(&self, _: &str, _: WorkflowPatch) -> Result<(), BoughError> {
            unreachable!()
        }
        fn create_workflow_agent(&self, _: WorkflowAgent) -> Result<WorkflowAgent, BoughError> {
            unreachable!()
        }
        fn update_workflow_agent(&self, _: &str, _: WorkflowAgentPatch) -> Result<(), BoughError> {
            unreachable!()
        }
        fn list_workflow_agents(&self, _: &str) -> Result<Vec<WorkflowAgent>, BoughError> {
            unreachable!()
        }
        fn find_workflow_agent(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<WorkflowAgent>, BoughError> {
            unreachable!()
        }
        fn record_command(&self, _: &CommandRecord) -> Result<(), BoughError> {
            unreachable!()
        }
        fn command_tag_rows(
            &self,
            _: &str,
            _: CommandTagOpts,
        ) -> Result<Vec<CommandTagRow>, BoughError> {
            unreachable!()
        }
        fn tag_spread(&self, _: Option<i64>) -> Result<(i64, HashMap<String, i64>), BoughError> {
            unreachable!()
        }
        fn tag_diversity_by_day(
            &self,
            _: i64,
            _: Option<&str>,
        ) -> Result<Vec<TagDiversityDay>, BoughError> {
            unreachable!()
        }
        fn commands_for_tag(
            &self,
            _: &str,
            _: Option<&str>,
            _: Option<i64>,
        ) -> Result<Vec<TaggedCommand>, BoughError> {
            unreachable!()
        }
        fn last_for_tags(
            &self,
            _: &[String],
            _: Option<&str>,
        ) -> Result<Vec<RecalledCommand>, BoughError> {
            unreachable!()
        }
        fn search_commands(
            &self,
            _: &str,
            _: &[String],
            _: i64,
        ) -> Result<Vec<TaggedCommand>, BoughError> {
            unreachable!()
        }
        fn repo_tag_counts(&self, _: &str, _: i64) -> Result<HashMap<String, i64>, BoughError> {
            unreachable!()
        }
        fn prior_failures(
            &self,
            _: &str,
            _: &str,
            _: i64,
            _: &str,
        ) -> Result<Option<PriorFailures>, BoughError> {
            unreachable!()
        }
        fn recent_failures(
            &self,
            _: &str,
            _: i64,
            _: i64,
        ) -> Result<Vec<RecentFailure>, BoughError> {
            unreachable!()
        }
        fn last_success_like(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: i64,
        ) -> Result<Option<String>, BoughError> {
            unreachable!()
        }
        fn program_for_message(&self, _: &str) -> Result<Option<String>, BoughError> {
            unreachable!()
        }
        fn index_message(&self, _: &Message) -> Result<(), BoughError> {
            unreachable!()
        }
        fn rebuild_search_index(&self) -> Result<(), BoughError> {
            unreachable!()
        }
        fn close(&self) {
            unreachable!()
        }
    }

    #[test]
    fn search_transcripts_skips_a_hit_whose_message_is_gone() {
        let raw = SqliteDb::new(":memory:", DbOptions::default()).unwrap();
        let s = raw
            .create_session(Session {
                id: uuid::Uuid::new_v4().to_string(),
                title: "drift".into(),
                kind: SessionKind::Root,
                created_at: tick(),
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
                description: None,
            })
            .unwrap();
        let m = raw
            .create_message(Message {
                id: uuid::Uuid::new_v4().to_string(),
                session_id: s.id,
                role: Role::User,
                parts: vec![Part::Text {
                    text: "a findable sentence".into(),
                }],
                pending: false,
                created_at: tick(),
            })
            .unwrap();
        raw.index_message(&m).unwrap();
        assert_eq!(
            raw.search_messages("findable", None, None).unwrap().len(),
            1
        );

        let drifting = DriftDb(raw);
        let result = search_transcripts(&drifting, "findable", SearchOpts::default()).unwrap();
        assert_eq!(result.count, 0);
        assert!(result.hits.is_empty());
    }
}
