//! Invariant: copying NEVER fails the caller (P3-D7). OSC52 is the copy path because it works over
//! SSH and inside the PTY the gate is measured in; `arboard` is best effort, and its failure is a
//! `notify` line rather than an error return.

use crate::TuiConfig;

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
