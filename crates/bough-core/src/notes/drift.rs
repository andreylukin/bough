//! Staleness, computed rather than guessed.
//!
//! WHY THIS IS POSSIBLE HERE AND NOT IN A WIKI. A personal knowledge base has
//! no fact stream to check a page against, so its only trigger for revision is
//! "a new source arrived" and its lint is structural — a page a year out of
//! date lints clean. Bough has `command_history`, so "is this note behind?" is
//! a COUNT, not a judgment, and no model is involved in asking it.
//!
//! THE FRONTIER IS PER HOST. The notes directory git-syncs between installs;
//! the command memory does not. Drift is therefore always measured against
//! THIS machine's frontier, over THIS machine's rows — see [`super::Note`].

use crate::errors::BoughError;
use crate::types::{Db, TaggedCommand};

/// How far behind one note is, on this host.
#[derive(Clone, Debug, PartialEq)]
pub struct Drift {
    pub key: String,
    /// Commands under this tag newer than this host's frontier.
    pub unfolded: usize,
    /// The newest `ts` seen, or `None` when nothing is unfolded. This is what
    /// a fold advances the frontier to — never `now`, so a fold that skipped
    /// rows cannot mark them accounted for.
    pub newest_ts: Option<i64>,
    /// Distinct sessions among the unfolded rows — the second half of the
    /// auto-create threshold.
    pub sessions: usize,
    /// Does the body carry an unresolved `> [!WARNING]`?
    pub warned: bool,
}

impl Drift {
    /// Sort key for the queue: warnings first, then by how far behind.
    pub fn severity(&self) -> (bool, usize) {
        (self.warned, self.unfolded)
    }
}

/// The unfolded commands for a tag: everything recorded after `since`, newest
/// first, bounded.
///
/// Reads the SAME rows `bough tags show` reads, deliberately — a note is an
/// interpretation of exactly the commands a human can go and look at.
pub fn unfolded_commands(
    db: &dyn Db,
    tag: &str,
    repo: Option<&str>,
    since: i64,
    limit: i64,
) -> Result<Vec<TaggedCommand>, BoughError> {
    let rows = db.commands_for_tag(tag, repo, Some(limit))?;
    Ok(rows.into_iter().filter(|r| r.ts > since).collect())
}

/// Measure one note against the memory.
pub fn drift_for(
    db: &dyn Db,
    key: &str,
    repo: Option<&str>,
    since: i64,
    warned: bool,
    limit: i64,
) -> Result<Drift, BoughError> {
    let rows = unfolded_commands(db, key, repo, since, limit)?;
    let mut sessions: Vec<&str> = rows.iter().map(|r| r.session_id.as_str()).collect();
    sessions.sort_unstable();
    sessions.dedup();
    Ok(Drift {
        key: key.to_string(),
        unfolded: rows.len(),
        newest_ts: rows.iter().map(|r| r.ts).max(),
        sessions: sessions.len(),
        warned,
    })
}

/// Has a reference earned a page yet?
///
/// The bar exists because the alternative is a page per tag: on a real memory
/// that is 1,971 files, 99.7% of them empty, an `index.md` nobody can read and
/// an orphan report that means nothing. Both halves matter — the session count
/// is what stops one long afternoon from minting a page for a reference that
/// never comes back.
pub fn earns_a_page(drift: &Drift) -> bool {
    drift.unfolded >= super::AUTO_CREATE_MIN_COMMANDS
        && drift.sessions >= super::AUTO_CREATE_MIN_SESSIONS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sqlite_db::{open_db, DbOptions};
    use crate::schema::parts::{Session, SessionKind};
    use crate::types::CommandRecord;

    fn db_with(rows: &[(&str, &str, i64, &str)]) -> Box<dyn Db> {
        let db = open_db(Some(":memory:"), DbOptions::default()).unwrap();
        let mut seen: Vec<String> = Vec::new();
        for (_, _, _, session) in rows {
            if seen.iter().any(|s| s == session) {
                continue;
            }
            seen.push(session.to_string());
            db.create_session(Session {
                id: session.to_string(),
                title: "t".into(),
                kind: SessionKind::Root,
                created_at: 0,
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
        }
        for (cmd, tags, ts, session) in rows {
            db.record_command(&CommandRecord {
                session_id: session.to_string(),
                ts: *ts,
                repo: "r".into(),
                cmd: cmd.to_string(),
                tags: tags.to_string(),
                tag_list: tags.split(':').map(str::to_string).collect(),
                exit_code: Some(0),
                duration_ms: Some(1),
                output_head: String::new(),
                spill_path: None,
                source: "live".into(),
                message_id: None,
                dirs: vec![],
            })
            .unwrap();
        }
        Box::new(db)
    }

    #[test]
    fn drift_counts_only_what_is_newer_than_the_frontier() {
        let db = db_with(&[
            ("a", "nased", 100, "s1"),
            ("b", "nased", 200, "s1"),
            ("c", "nased", 300, "s2"),
        ]);
        let d = drift_for(&*db, "nased", Some("r"), 150, false, 100).unwrap();
        assert_eq!(d.unfolded, 2);
        assert_eq!(d.newest_ts, Some(300));
        assert_eq!(d.sessions, 2);

        let caught_up = drift_for(&*db, "nased", Some("r"), 300, false, 100).unwrap();
        assert_eq!(caught_up.unfolded, 0);
        assert_eq!(
            caught_up.newest_ts, None,
            "nothing unfolded means there is no frontier to advance to"
        );
    }

    #[test]
    fn a_reference_earns_a_page_only_on_both_halves() {
        let one_session = Drift {
            key: "linear.x-1".into(),
            unfolded: 50,
            newest_ts: Some(1),
            sessions: 1,
            warned: false,
        };
        assert!(!earns_a_page(&one_session), "one afternoon is not a topic");

        let thin = Drift {
            sessions: 5,
            unfolded: 3,
            ..one_session.clone()
        };
        assert!(!earns_a_page(&thin), "three commands is not a topic either");

        let real = Drift {
            unfolded: 20,
            sessions: 2,
            ..one_session
        };
        assert!(earns_a_page(&real));
    }

    #[test]
    fn the_queue_puts_warnings_above_volume() {
        let warned = Drift {
            key: "a".into(),
            unfolded: 1,
            newest_ts: Some(1),
            sessions: 1,
            warned: true,
        };
        let busy = Drift {
            key: "b".into(),
            warned: false,
            unfolded: 999,
            ..warned.clone()
        };
        assert!(warned.severity() > busy.severity());
    }
}
