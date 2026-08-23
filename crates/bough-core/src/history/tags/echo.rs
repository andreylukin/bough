//! The command memory, PUSHED (port of `src/history/echo.ts`) — what it
//! already knows, delivered without being asked.
//!
//! WHY THIS EXISTS. Three days of live use measured 100% of commands written,
//! ~60 recall reads against ~3,900 writes: the memory is not weak — it is
//! unconsulted, because consulting it is a decision the model has to remember
//! to make mid-task, and it does not. So this module is the memory speaking
//! first, at the only two moments it can:
//!
//!   - [`CommandEcho::note`] — AFTER a command fails, append what this
//!     command, and this MISTAKE, have already done here, plus the nearest
//!     thing that exited 0.
//!   - [`CommandEcho::guard`] — BEFORE a command runs, refuse a command
//!     already failing in a tight loop and hand back the error it is about
//!     to produce again.
//!
//! THE CORRECTION, AND WHY THIS FILE HAS TWO MATCHERS. The motivating
//! incident was one hundred `gh search prs … --state merged` calls — one
//! hundred DIFFERENT command strings (one per ticket), every one failing with
//! `invalid argument "merged"`. Byte-exact matching fires ZERO times on it.
//! The real failure mode is one misconception applied across varying
//! commands, so recall also groups by ERROR: what the command printed, first
//! line, whatever it was called.
//!
//! THE ERROR PATH NOTES BUT NEVER GUARDS, deliberately. Three different
//! commands hitting the same error is what debugging looks like from the
//! outside — refusing the fourth attempt would break exactly the loop that
//! fixes things. Command identity is the only case where "nothing can
//! change" is provable, so it stays the only case that refuses.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::types::{system_clock, Clock, SharedDb};

use super::record::attribute_command;

/// How far back a repeat failure is worth mentioning at all.
const ECHO_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1000;
/// The loop window: failures this close together are one runaway, not history.
const LOOP_WINDOW_MS: i64 = 2 * 60 * 1000;
/// Failures of the identical command, in this session, inside the loop window.
const LOOP_THRESHOLD: i64 = 3;
/// Enough of the last failure to recognise it; a full 2k head would bury the
/// note.
const ERROR_CHARS: usize = 220;
/// Leading tokens that define "the same kind of command" for a success lookup.
const PREFIX_TOKENS: usize = 2;
/// How far back an error signature is worth grouping over. A day of work.
const ERROR_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// Failing rows scanned for a signature match. Bounds the cost of a busy repo.
const ERROR_SCAN_LIMIT: i64 = 400;
/// DISTINCT commands that must already have produced this error before it is
/// worth saying so. Two is one repetition — the point at which "the command
/// changed but the mistake did not" is a fact rather than a coincidence.
const ERROR_SPREAD_MIN: usize = 2;

pub struct EchoCtx {
    pub db: SharedDb,
    pub session_id: String,
    pub workspace: String,
    pub now: Option<Clock>,
}

/// The first line of what a command printed, trimmed of a trailing
/// `[exit code N]`. Clipped to [`ERROR_CHARS`] chars with `…`.
pub(crate) fn first_error_line(output_head: &str) -> String {
    static EXIT_LINE: OnceLock<regex::Regex> = OnceLock::new();
    let exit_line = EXIT_LINE.get_or_init(|| regex::Regex::new(r"^\[exit code -?\d+\]$").unwrap());
    let Some(line) = output_head
        .split('\n')
        .map(str::trim)
        .find(|l| !l.is_empty() && !exit_line.is_match(l))
    else {
        return String::new();
    };
    if line.chars().count() > ERROR_CHARS {
        format!("{}…", line.chars().take(ERROR_CHARS).collect::<String>())
    } else {
        line.to_string()
    }
}

/// `4m ago`, `2s ago` — the same vocabulary `bough tags show` prints.
pub(crate) fn ago(ms: i64) -> String {
    let s = ((ms as f64) / 1000.0).round().max(0.0);
    if s < 60.0 {
        return format!("{}s ago", s as i64);
    }
    let m = (s / 60.0).round();
    if m < 60.0 {
        return format!("{}m ago", m as i64);
    }
    let h = (m / 60.0).round();
    if h < 48.0 {
        format!("{}h ago", h as i64)
    } else {
        format!("{}d ago", (h / 24.0).round() as i64)
    }
}

/// The LIKE pattern for "a command of this kind": the first couple of tokens,
/// with LIKE's own wildcards escaped so a command containing `%` cannot widen
/// its own search. None when there is nothing distinctive to match on.
pub(crate) fn success_prefix(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().take(PREFIX_TOKENS).collect();
    if tokens.is_empty() {
        return None;
    }
    let prefix = tokens.join(" ");
    if prefix.chars().count() < 2 {
        return None;
    }
    let mut out = String::with_capacity(prefix.len());
    for c in prefix.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    Some(out)
}

/// The per-turn echo. Every failure is swallowed for the same reason the
/// recorder swallows its own: recall is a side channel, and a broken lookup
/// must never be a broken round.
pub struct CommandEcho {
    db: SharedDb,
    session_id: String,
    workspace: String,
    now: Clock,
}

pub fn create_command_echo(ctx: EchoCtx) -> CommandEcho {
    CommandEcho {
        db: ctx.db,
        session_id: ctx.session_id,
        workspace: ctx.workspace,
        now: ctx.now.unwrap_or_else(system_clock),
    }
}

impl CommandEcho {
    /// Repo attribution is not free (it stats paths), and both entry points
    /// want it.
    fn repo_for(&self, command: &str) -> String {
        attribute_command(command, &self.workspace).repo
    }

    /// The note to append to a finished command's output, or None. Called
    /// with what the command actually did, so a success is cheap: it returns
    /// immediately.
    pub fn note(&self, command: &str, exit_code: Option<i64>, output: &str) -> Option<String> {
        // A success has nothing to be warned about, and this is the common
        // case. A still-running command (None) has no verdict yet.
        if exit_code == Some(0) || exit_code.is_none() {
            return None;
        }
        let at = (self.now)();
        let repo = self.repo_for(command);
        let db = self.db.lock().ok()?;
        let mut lines: Vec<String> = Vec::new();

        // (1) THIS COMMAND, before. The narrow, certain case.
        let prior = db
            .prior_failures(&repo, command, at - ECHO_WINDOW_MS, &self.session_id)
            .ok()?;
        if let Some(prior) = &prior {
            let times = if prior.count == 1 {
                "once".to_string()
            } else {
                format!("{}×", prior.count)
            };
            lines.push(format!(
                "[history] this exact command already failed here {times} (last {}): {}",
                ago(at - prior.last_ts),
                first_error_line(&prior.output_head)
            ));
        }

        // (2) THIS MISTAKE, before — across whatever commands carried it. The
        // case that the byte-exact matcher above provably cannot see, and the
        // one the motivating incident actually was.
        let signature = first_error_line(output);
        if !signature.is_empty() {
            let mut seen: HashSet<String> = HashSet::new();
            let mut last = 0i64;
            for f in db
                .recent_failures(&repo, at - ERROR_WINDOW_MS, ERROR_SCAN_LIMIT)
                .ok()?
            {
                if f.cmd == command || first_error_line(&f.output_head) != signature {
                    continue;
                }
                seen.insert(f.cmd);
                if f.ts > last {
                    last = f.ts;
                }
            }
            if seen.len() >= ERROR_SPREAD_MIN {
                lines.push(format!(
                    "[history] {} other commands here failed the same way (last {}): {signature}",
                    seen.len(),
                    ago(at - last)
                ));
                lines.push(
                    "          The command has been changing; the mistake has not. Fix the \
                     mistake, not the arguments."
                        .to_string(),
                );
            }
        }

        if lines.is_empty() {
            return None;
        }
        if let Some(prefix) = success_prefix(command) {
            if let Ok(Some(worked)) =
                db.last_success_like(&repo, &prefix, command, at - ECHO_WINDOW_MS)
            {
                lines.push(format!("          this exited 0 here: {worked}"));
            }
        }
        Some(lines.join("\n"))
    }

    /// The output to return INSTEAD of running the command, or None to run
    /// it. A non-null answer means nothing was spawned. Fires ONLY on: same
    /// session + byte-identical command + ≥3 failures inside 2 minutes.
    pub fn guard(&self, command: &str) -> Option<String> {
        let at = (self.now)();
        let repo = self.repo_for(command);
        let db = self.db.lock().ok()?;
        // Scoped to the loop window, not the echo window: an old failure must
        // not count toward a runaway, and this is the query that decides a
        // refusal.
        let prior = db
            .prior_failures(&repo, command, at - LOOP_WINDOW_MS, &self.session_id)
            .ok()??;
        if prior.in_session < LOOP_THRESHOLD {
            return None;
        }
        Some(
            [
                format!(
                    "[not run] this identical command has failed {} times in this session in \
                     the last {} minutes, so it was skipped rather than run a {}th time.",
                    prior.in_session,
                    LOOP_WINDOW_MS / 60_000,
                    prior.in_session + 1
                ),
                String::new(),
                format!("Its last error, {}:", ago(at - prior.last_ts)),
                format!("  {}", first_error_line(&prior.output_head)),
                String::new(),
                "Change the command and it runs — any edit makes it a different command. To \
                 see what has worked here: bough tags show <tag>, or bough tags sql \"SELECT \
                 cmd FROM command_history WHERE exit_code = 0 AND cmd LIKE '…' ORDER BY ts \
                 DESC LIMIT 5\"."
                    .to_string(),
            ]
            .join("\n"),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from src/history/echo.test.ts.
//
// The behaviours worth pinning are the ones a future edit could quietly
// invert:
//
//   - **A first failure says nothing.** The echo is history, not commentary.
//   - **The guard is byte-exact and session-scoped.** It refuses to run
//     something, which is the one thing here that can be wrong in a way that
//     costs the user a turn.
//
// Hermetic: `:memory:`, an injected clock, and a workspace path that is not
// a git checkout, so `repo_identity` resolves to the path.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{DbOptions, SqliteDb};
    use crate::schema::parts::{Session, SessionKind};
    use crate::types::{CommandRecord, Db};
    use std::sync::{Arc, Mutex};

    const WS: &str = "/nonexistent-workspace-for-echo-tests";
    const SESSION: &str = "s1";
    const T0: i64 = 1_700_000_000_000;
    /// What every failing command in these tests printed.
    const ERR: &str = "invalid argument \"merged\"\n[exit code 1]";

    struct FailOpts {
        ts: i64,
        session: Option<&'static str>,
        out: Option<&'static str>,
        exit_code: Option<i64>,
    }

    fn at(ts: i64) -> FailOpts {
        FailOpts {
            ts,
            session: None,
            out: None,
            exit_code: Some(1),
        }
    }

    fn record(db: &SharedDb, cmd: &str, opts: FailOpts) {
        db.lock()
            .unwrap()
            .record_command(&CommandRecord {
                session_id: opts.session.unwrap_or(SESSION).to_string(),
                ts: opts.ts,
                repo: WS.to_string(),
                cmd: cmd.to_string(),
                tags: String::new(),
                tag_list: vec![],
                dirs: vec![],
                exit_code: opts.exit_code,
                duration_ms: Some(5),
                output_head: opts.out.unwrap_or(ERR).to_string(),
                spill_path: None,
                source: "live".to_string(),
                message_id: None,
            })
            .unwrap();
    }

    fn fail(db: &SharedDb, cmd: &str, opts: FailOpts) {
        record(db, cmd, opts);
    }

    fn ok_cmd(db: &SharedDb, cmd: &str, ts: i64, out: &'static str) {
        record(
            db,
            cmd,
            FailOpts {
                ts,
                session: None,
                out: Some(out),
                exit_code: Some(0),
            },
        );
    }

    /// A database with the sessions the history rows below hang off.
    fn fresh_db() -> SharedDb {
        let db = SqliteDb::new(":memory:", DbOptions::default()).unwrap();
        for id in [SESSION, "someone-else"] {
            db.create_session(Session {
                id: id.to_string(),
                title: id.to_string(),
                kind: SessionKind::Root,
                created_at: T0 - 1_000_000,
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
        }
        Arc::new(Mutex::new(db))
    }

    fn echo_over(db: &SharedDb) -> CommandEcho {
        create_command_echo(EchoCtx {
            db: db.clone(),
            session_id: SESSION.to_string(),
            workspace: WS.to_string(),
            now: Some(Arc::new(|| T0)),
        })
    }

    #[test]
    fn a_command_with_no_failing_history_gets_no_note() {
        let db = fresh_db();
        let echo = echo_over(&db);
        assert_eq!(
            echo.note("gh search prs --state merged", Some(1), ERR),
            None
        );
    }

    #[test]
    fn a_repeat_failure_is_echoed_with_the_count_and_the_last_error() {
        let db = fresh_db();
        fail(&db, "gh search prs --state merged", at(T0 - 60_000));
        fail(&db, "gh search prs --state merged", at(T0 - 30_000));
        let note = echo_over(&db)
            .note("gh search prs --state merged", Some(1), ERR)
            .expect("expected a note");
        assert!(note.contains("already failed here 2×"), "{note}");
        assert!(note.contains("invalid argument \"merged\""), "{note}");
        // The `[exit code N]` trailer is the harness talking, not the error.
        assert!(!note.contains("[exit code"), "{note}");
    }

    #[test]
    fn a_successful_sibling_command_is_offered_alongside_the_failure() {
        let db = fresh_db();
        fail(&db, "gh search prs --state merged", at(T0 - 30_000));
        ok_cmd(
            &db,
            "gh search prs --state closed --json number",
            T0 - 20_000,
            "[]",
        );
        let note = echo_over(&db)
            .note("gh search prs --state merged", Some(1), ERR)
            .expect("expected a note");
        assert!(
            note.contains("this exited 0 here: gh search prs --state closed --json number"),
            "{note}"
        );
    }

    #[test]
    fn a_success_is_never_echoed_however_bad_its_history() {
        let db = fresh_db();
        fail(&db, "flaky", at(T0 - 10_000));
        fail(&db, "flaky", at(T0 - 5_000));
        assert_eq!(echo_over(&db).note("flaky", Some(0), ERR), None);
        // A still-running command (no verdict) is not a failure either.
        assert_eq!(echo_over(&db).note("flaky", None, ERR), None);
    }

    #[test]
    fn the_guard_fires_only_at_the_threshold_and_quotes_what_it_skipped() {
        let db = fresh_db();
        let cmd = "gh search prs --state merged";
        let echo = echo_over(&db);
        fail(&db, cmd, at(T0 - 3_000));
        assert_eq!(echo.guard(cmd), None, "one failure is not a loop");
        fail(&db, cmd, at(T0 - 2_000));
        assert_eq!(echo.guard(cmd), None, "two failures are not a loop");
        fail(&db, cmd, at(T0 - 1_000));
        let skip = echo
            .guard(cmd)
            .expect("three identical failures in seconds is a loop");
        assert!(skip.starts_with("[not run]"), "{skip}");
        assert!(skip.contains("failed 3 times in this session"), "{skip}");
        assert!(skip.contains("invalid argument \"merged\""), "{skip}");
    }

    #[test]
    fn the_guard_ignores_older_failures_other_sessions_and_edited_commands() {
        let db = fresh_db();
        let cmd = "gh search prs --state merged";
        for _ in 0..3 {
            fail(&db, cmd, at(T0 - 10 * 60_000));
        }
        let echo = echo_over(&db);
        assert_eq!(
            echo.guard(cmd),
            None,
            "ten minutes ago is history, not a loop"
        );

        for ts in [T0 - 3_000, T0 - 2_000, T0 - 1_000] {
            fail(
                &db,
                cmd,
                FailOpts {
                    ts,
                    session: Some("someone-else"),
                    out: None,
                    exit_code: Some(1),
                },
            );
        }
        assert_eq!(
            echo.guard(cmd),
            None,
            "another session's loop is not this one's"
        );

        for ts in [T0 - 3_000, T0 - 2_000, T0 - 1_000] {
            fail(&db, cmd, at(ts));
        }
        assert!(
            echo.guard(cmd).is_some(),
            "this session's own loop does fire"
        );
        assert_eq!(
            echo.guard(&format!("{cmd} --json number")),
            None,
            "any edit makes it a different command"
        );
    }

    #[test]
    fn a_command_containing_like_wildcards_cannot_widen_its_own_success_lookup() {
        let db = fresh_db();
        fail(&db, "rg %_ src", at(T0 - 30_000));
        ok_cmd(&db, "rg unrelated-thing src", T0 - 20_000, "");
        let note = echo_over(&db)
            .note("rg %_ src", Some(1), ERR)
            .expect("expected a note");
        assert!(
            !note.contains("unrelated-thing"),
            "`%_` must be escaped, not matched as wildcards: {note}"
        );
    }

    #[test]
    fn a_broken_lookup_is_silent_never_a_thrown_round() {
        // A db whose command tables are gone: every query errors, and both
        // entry points must swallow that. The tables are dropped through a
        // SECOND connection to the same file, so the handle under test holds
        // no raw-SQL escape hatch.
        let path =
            std::env::temp_dir().join(format!("bough-echo-broken-{}.db", uuid::Uuid::new_v4()));
        let path_s = path.to_string_lossy().into_owned();
        let under_test = SqliteDb::new(&path_s, DbOptions::default()).unwrap();
        {
            let saboteur = rusqlite::Connection::open(&path_s).unwrap();
            saboteur
                .execute_batch(
                    "DROP TABLE command_history_fts; DROP TABLE command_tags; \
                     DROP TABLE command_dirs; DROP TABLE command_history;",
                )
                .unwrap();
        }
        let db: SharedDb = Arc::new(Mutex::new(under_test));
        let echo = echo_over(&db);
        assert_eq!(echo.note("anything", Some(1), ERR), None);
        assert_eq!(echo.guard("anything"), None);
        let _ = std::fs::remove_file(&path);
    }

    // ---- the error path — the incident this was actually built for --------

    #[test]
    fn the_real_incident_a_hundred_distinct_commands_one_mistake() {
        // Reconstructed from the rows that motivated this module. Every
        // command differs (one ticket each), every command fails identically.
        // Command-identity matching sees nothing here — that is the whole
        // point of the error path.
        let db = fresh_db();
        let tickets = ["NMC-5630", "NMFB-1811", "NMC-5602", "NMC-5881"];
        let cmd_for = |t: &str| {
            format!(
                "gh search prs \"{t}\" --owner uni-intelligence --state merged --json number \
                 --limit 20"
            )
        };
        for (i, t) in tickets.iter().enumerate() {
            fail(&db, &cmd_for(t), at(T0 - (40 - i as i64) * 1_000));
        }
        let echo = echo_over(&db);
        let next = cmd_for("NMC-9999");

        // The byte-exact matchers are blind to it, exactly as they were in
        // production.
        assert_eq!(
            echo.guard(&next),
            None,
            "distinct commands are not a stuck loop"
        );
        let note = echo
            .note(&next, Some(1), ERR)
            .expect("the error path must see what command identity cannot");
        assert!(
            note.contains("4 other commands here failed the same way"),
            "{note}"
        );
        assert!(note.contains("invalid argument \"merged\""), "{note}");
        assert!(
            note.contains("The command has been changing; the mistake has not"),
            "{note}"
        );
        assert!(
            !note.contains("this exact command"),
            "no command ran twice: {note}"
        );
    }

    #[test]
    fn one_other_command_with_the_same_error_is_not_yet_a_pattern() {
        let db = fresh_db();
        fail(&db, "gh pr list --state merged", at(T0 - 5_000));
        assert_eq!(
            echo_over(&db).note("gh search prs --state merged", Some(1), ERR),
            None
        );
    }

    #[test]
    fn a_different_error_does_not_group_however_many_commands_failed() {
        let db = fresh_db();
        for i in 0..6 {
            let out: &'static str = Box::leak(
                format!("connection refused on port {i}\n[exit code 1]").into_boxed_str(),
            );
            fail(
                &db,
                &format!("cmd-{i}"),
                FailOpts {
                    ts: T0 - 5_000,
                    session: None,
                    out: Some(out),
                    exit_code: Some(1),
                },
            );
        }
        // Same repo, plenty of failures, unrelated mistake.
        assert_eq!(
            echo_over(&db).note("gh search prs --state merged", Some(1), ERR),
            None
        );
    }

    #[test]
    fn both_matchers_can_speak_at_once_command_first() {
        let db = fresh_db();
        let cmd = "gh search prs --state merged";
        fail(&db, cmd, at(T0 - 9_000));
        fail(
            &db,
            "gh search prs --owner x --state merged",
            at(T0 - 8_000),
        );
        fail(
            &db,
            "gh search prs --owner y --state merged",
            at(T0 - 7_000),
        );
        let note = echo_over(&db).note(cmd, Some(1), ERR).expect("a note");
        let exact = note.find("this exact command");
        let spread = note.find("other commands here failed the same way");
        assert!(
            exact.is_some() && spread.is_some(),
            "both lines present: {note}"
        );
        assert!(
            exact < spread,
            "the certain fact comes before the inferred pattern"
        );
    }

    #[test]
    fn a_command_that_printed_nothing_has_no_signature_to_group_on() {
        let db = fresh_db();
        for i in 0..4 {
            fail(
                &db,
                &format!("q-{i}"),
                FailOpts {
                    ts: T0 - 5_000,
                    session: None,
                    out: Some(""),
                    exit_code: Some(1),
                },
            );
        }
        assert_eq!(echo_over(&db).note("q-new", Some(1), ""), None);
    }
}
