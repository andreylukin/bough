//! Which conversation you were in, per workspace.
//!
//! THE FRICTION THIS REMOVES. Restarting the harness — to pick up a rebuild,
//! to clear a wedged server — meant quitting, starting again, and then finding
//! your conversation by hand in the tree. Nothing about that was a decision;
//! it was three steps of re-establishing a state the process already knew.
//!
//! So the last conversation opened in a workspace is recorded, and `--resume`
//! reopens it. Per WORKSPACE, not globally: two checkouts are two lines of
//! work, and resuming one into the other's conversation is worse than not
//! resuming at all.
//!
//! ## It records, it does not decide
//!
//! Bare `bough` still opens fresh. Reopening a conversation is a thing you can
//! ask for; a harness that silently drops you back into last week's thread —
//! with its context, its costs and its half-finished plan — has made a
//! decision that was not its to make.
//!
//! Best-effort throughout: a file that cannot be written or parsed costs the
//! convenience and nothing else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::paths::bough_path;

pub fn resume_path() -> PathBuf {
    bough_path(&["last-session.json"])
}

fn read(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Record `session_id` as the conversation open in `workspace`.
pub fn remember(workspace: &str, session_id: &str) {
    remember_at(&resume_path(), workspace, session_id)
}

pub fn remember_at(path: &Path, workspace: &str, session_id: &str) {
    if workspace.is_empty() || session_id.is_empty() {
        return;
    }
    let mut map = read(path);
    if map.get(workspace).map(String::as_str) == Some(session_id) {
        return; // already there; do not rewrite the file on every turn
    }
    map.insert(workspace.to_string(), session_id.to_string());
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(&map).unwrap_or_default());
}

/// The conversation last open in `workspace`, if any.
pub fn last_for(workspace: &str) -> Option<String> {
    last_for_at(&resume_path(), workspace)
}

pub fn last_for_at(path: &Path, workspace: &str) -> Option<String> {
    read(path).get(workspace).cloned()
}

/// Drop a workspace's record — for a session that no longer exists, so the
/// next `--resume` opens fresh instead of failing on a dead id.
pub fn forget(workspace: &str) {
    let path = resume_path();
    let mut map = read(&path);
    if map.remove(workspace).is_some() {
        let _ = std::fs::write(
            &path,
            serde_json::to_string_pretty(&map).unwrap_or_default(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!("bough-resume-{}.json", uuid::Uuid::new_v4()))
    }

    #[test]
    fn what_was_open_here_comes_back_and_another_workspace_is_untouched() {
        let path = temp();
        remember_at(&path, "/repos/bough", "session-a");
        remember_at(&path, "/repos/other", "session-b");
        assert_eq!(
            last_for_at(&path, "/repos/bough").as_deref(),
            Some("session-a")
        );
        assert_eq!(
            last_for_at(&path, "/repos/other").as_deref(),
            Some("session-b"),
            "two checkouts are two lines of work"
        );
        assert_eq!(last_for_at(&path, "/repos/never-seen"), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_newest_conversation_in_a_workspace_wins() {
        let path = temp();
        remember_at(&path, "/w", "old");
        remember_at(&path, "/w", "new");
        assert_eq!(last_for_at(&path, "/w").as_deref(), Some("new"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_or_corrupt_file_is_no_resume_rather_than_an_error() {
        let path = temp();
        assert_eq!(last_for_at(&path, "/w"), None);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(last_for_at(&path, "/w"), None);
        // And it is still writable afterwards: a corrupt file is replaced, not
        // treated as a state this has to preserve.
        remember_at(&path, "/w", "s1");
        assert_eq!(last_for_at(&path, "/w").as_deref(), Some("s1"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn blanks_are_never_recorded() {
        let path = temp();
        remember_at(&path, "", "s1");
        remember_at(&path, "/w", "");
        assert!(!path.exists(), "nothing worth writing was written");
    }
}
