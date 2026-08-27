//! Invariant: copying NEVER fails the caller (P3-D7). OSC52 is the copy path because it works over
//! SSH and inside the PTY the gate is measured in; `arboard` is best effort, and its failure is a
//! `notify` line rather than an error return.

use crate::{NoticeKind, TuiConfig};

/// The word a successful copy flashes. One word, so it fits beside a selection at 80 columns.
pub const COPIED: &str = "copied";

/// The hint shown beside a copy the terminal could not take, so the user has a way out that does
/// not involve the app at all (M21).
pub const SHIFT_DRAG: &str = "shift-drag to select with the terminal";

/// What actually happened to the copy.
#[derive(Clone, Debug, PartialEq)]
pub enum CopyOutcome {
    Osc52AndLocal,
    Osc52Only,
    LocalOnly,
    /// Nothing was copied, and this is why. Rendered as a notice.
    Nothing(String),
}

impl CopyOutcome {
    /// The FLASH every copy shows (phase ux1 §2.10, M21). Unlike [`CopyOutcome::notice`] this is
    /// never `None`: the audit's finding is that a successful copy said nothing at all, so the
    /// user could not tell a working copy from a dead keybinding. A success flashes `copied` in
    /// the `Copied` role (it fades on `flash_ms`); a failure is an `Error` and names the way out.
    pub fn flash(&self) -> (String, NoticeKind) {
        match self {
            // The hint rides along with the SUCCESS too: a reader who wanted their terminal's
            // own selection (to paste elsewhere, to keep the scrollback) has no other way to
            // learn that the mouse grab can be stepped around (M21).
            CopyOutcome::Osc52AndLocal | CopyOutcome::Osc52Only => {
                (format!("{COPIED} — {SHIFT_DRAG}"), NoticeKind::Copied)
            }
            CopyOutcome::LocalOnly => (
                format!("{COPIED} (terminal refused OSC52) — {SHIFT_DRAG}"),
                NoticeKind::Copied,
            ),
            CopyOutcome::Nothing(why) => (
                format!("nothing copied: {why} — {SHIFT_DRAG}"),
                NoticeKind::Error,
            ),
        }
    }

    /// The one-line notice a surface shows. `None` when the copy needs no comment.
    pub fn notice(&self) -> Option<String> {
        match self {
            CopyOutcome::Osc52AndLocal | CopyOutcome::Osc52Only => None,
            CopyOutcome::LocalOnly => Some("copied (terminal refused OSC52)".to_string()),
            CopyOutcome::Nothing(why) => Some(format!("nothing copied: {why}")),
        }
    }
}

/// OSC52 first (crossterm's `clipboard::CopyToClipboard`, feature `osc52`), then `arboard` when
/// `clipboard: true`. An `arboard` failure is a `notify` line, never an error: a PTY has no
/// display server and must still copy (P3-D7).
pub async fn copy(text: &str, cfg: &TuiConfig, out: &mut impl std::io::Write) -> CopyOutcome {
    if text.is_empty() {
        return CopyOutcome::Nothing("the selection is empty".to_string());
    }

    let osc52 = if cfg.osc52 {
        write_osc52(text, out).is_ok()
    } else {
        false
    };

    let local = if cfg.clipboard {
        // `arboard` opens a display connection, which blocks; it never runs on the loop's thread.
        let owned = text.to_string();
        match tokio::task::spawn_blocking(move || {
            arboard::Clipboard::new()
                .and_then(|mut c| c.set_text(owned))
                .map_err(|e| e.to_string())
        })
        .await
        {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.to_string()),
        }
    } else {
        Err("local clipboard is off".to_string())
    };

    match (osc52, local.is_ok()) {
        (true, true) => CopyOutcome::Osc52AndLocal,
        (true, false) => CopyOutcome::Osc52Only,
        (false, true) => CopyOutcome::LocalOnly,
        (false, false) => CopyOutcome::Nothing(match local {
            Err(e) if cfg.clipboard => e,
            _ => "osc52 is off and no local clipboard is configured".to_string(),
        }),
    }
}

/// The OSC52 sequence itself, so a test can assert against a `Vec<u8>` writer with no terminal.
pub fn write_osc52(text: &str, out: &mut impl std::io::Write) -> std::io::Result<()> {
    use crossterm::clipboard::CopyToClipboard;
    crossterm::queue!(out, CopyToClipboard::to_clipboard_from(text))?;
    out.flush()
}

#[cfg(test)]
mod flash_tests {
    use super::*;

    #[test]
    fn every_copy_flashes_something() {
        for o in [
            CopyOutcome::Osc52AndLocal,
            CopyOutcome::Osc52Only,
            CopyOutcome::LocalOnly,
            CopyOutcome::Nothing("no clipboard".into()),
        ] {
            let (text, _) = o.flash();
            assert!(!text.is_empty(), "{o:?} said nothing");
        }
    }

    #[test]
    fn a_success_flashes_copied_in_the_copied_role() {
        let (text, kind) = CopyOutcome::Osc52AndLocal.flash();
        assert!(text.starts_with(COPIED), "{text}");
        // …and it carries the escape hatch, which a success is the only moment anyone reads.
        assert!(text.contains(SHIFT_DRAG), "{text}");
        assert_eq!(kind, NoticeKind::Copied);
    }

    /// A copy that could not happen is an ERROR, and it names the terminal's own way out.
    #[test]
    fn a_failure_is_an_error_and_offers_shift_drag() {
        let (text, kind) = CopyOutcome::Nothing("osc52 is off".into()).flash();
        assert_eq!(kind, NoticeKind::Error);
        assert!(text.contains("osc52 is off"), "{text}");
        assert!(text.contains(SHIFT_DRAG), "{text}");
    }
}
