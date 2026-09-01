//! Invariant: ONE client at a time, and the ACTIVE one is the only reader of the shell's frames
//! and the only writer of its events. A later attach detaches the earlier client with a named
//! reason; a session that ends cleans up only what is still its own (the registration and the
//! byte sink are seq-guarded), so a stale session can never take the live one's wiring down.
//!
//! The session re-renders the shell's published `last_frame` through its own diffing
//! `Terminal<CrosstermBackend<_>>` over an in-memory writer, so what crosses the socket is the
//! same ANSI a local terminal would have been sent — and only the cells that changed.

use std::sync::Arc;

use bough_plugin_tui_shell::TuiHandle;
use crossterm::event::Event;
use parking_lot::Mutex;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::proto::{self, ClientHello, Exit, ServerHello};

/// The ANSI the blit terminal writes between drains. `io::Write` over a shared Vec, because
/// `CrosstermBackend` owns its writer and the session still has to take the bytes out.
#[derive(Clone, Default)]
pub struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    /// Take everything written since the last drain.
    pub fn drain(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock())
    }
}

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The blit terminal at one client size. `Viewport::Fixed` is what keeps ratatui from asking the
/// backend for a size this process's missing tty cannot answer.
pub fn blit_terminal(
    buf: SharedBuf,
    cols: u16,
    rows: u16,
) -> std::io::Result<Terminal<CrosstermBackend<SharedBuf>>> {
    Terminal::with_options(
        CrosstermBackend::new(buf),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, cols.max(1), rows.max(1))),
        },
    )
}

/// Copy the shell's published buffer into the blit terminal's current frame. The terminal diffs
/// against its previous frame, so the backend — and therefore the socket — sees changed cells
/// only. Sizes may disagree for a frame around a resize; the copy clamps to the overlap.
pub fn blit(
    term: &mut Terminal<CrosstermBackend<SharedBuf>>,
    src: &ratatui::buffer::Buffer,
) -> std::io::Result<()> {
    term.draw(|f| {
        let dst_area = f.area();
        let buf = f.buffer_mut();
        let overlap = dst_area.intersection(src.area);
        for y in overlap.y..overlap.bottom() {
            for x in overlap.x..overlap.right() {
                buf[(x, y)] = src[(x, y)].clone();
            }
        }
    })?;
    Ok(())
}

/// Which session currently owns the client wiring, seq-guarded so a stale session's cleanup
/// cannot touch a newer session's registration.
#[derive(Default)]
struct Registry {
    seq: u64,
    active: Option<mpsc::UnboundedSender<Exit>>,
    /// Set when the listener is going away: a handshake that finishes after that is turned away
    /// instead of registering into a row that is already being disposed.
    closed: bool,
}

/// The row's shared state: the registry, plus what `/detach` needs.
#[derive(Default)]
pub struct AttachState {
    inner: Mutex<Registry>,
}

impl AttachState {
    /// Register a freshly-handshaken session. The previous client, if any, is told why it is
    /// going. `None` means the listener is closed and the session must end instead.
    fn register(&self, tx: mpsc::UnboundedSender<Exit>) -> Option<u64> {
        let mut r = self.inner.lock();
        if r.closed {
            return None;
        }
        r.seq += 1;
        if let Some(old) = r.active.replace(tx) {
            let _ = old.send(Exit {
                code: 0,
                reason: "detached: another bough took this home (one terminal at a time). \
                         Run `bough` here to take it back."
                    .to_string(),
            });
        }
        Some(r.seq)
    }

    /// End a session's registration. `true` only when the seq is still the live one — the caller
    /// then owns tearing the shared wiring (the byte sink) down.
    fn end(&self, seq: u64) -> bool {
        let mut r = self.inner.lock();
        if r.seq == seq && r.active.is_some() {
            r.active = None;
            true
        } else {
            false
        }
    }

    /// Detach the current client, with a reason. `true` when there was one.
    pub fn detach(&self, code: u8, reason: &str) -> bool {
        let taken = self.inner.lock().active.take();
        match taken {
            Some(tx) => {
                let _ = tx.send(Exit {
                    code,
                    reason: reason.to_string(),
                });
                true
            }
            None => false,
        }
    }

    /// Close the registry: detach the current client and refuse later registrations.
    pub fn close(&self, reason: &str) {
        self.inner.lock().closed = true;
        self.detach(0, reason);
    }

    /// Whether a client is attached right now.
    pub fn attached(&self) -> bool {
        self.inner.lock().active.is_some()
    }
}

/// What the reader half forwards to the session loop. A separate task because `read_frame` is not
/// cancel-safe inside a `select!` — a cancelled half-read would tear the stream.
enum ClientMsg {
    Event(Event),
    Gone(Option<proto::ProtoError>),
}

/// One attached client, end to end: handshake, register (stealing), pump frames out and events
/// in, clean up only what is still ours.
pub async fn session(
    stream: UnixStream,
    tui: TuiHandle,
    state: Arc<AttachState>,
    handshake_ms: u64,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut read_half = read_half;

    // The hello, under a deadline: a connection that never says hello must not hold the slot.
    let hello: ClientHello = match tokio::time::timeout(
        std::time::Duration::from_millis(handshake_ms),
        proto::read_frame(&mut read_half),
    )
    .await
    {
        Ok(Ok(Some((proto::C_HELLO, payload)))) => match proto::decode("hello", &payload) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "tui-attach: a client sent a malformed hello");
                return;
            }
        },
        other => {
            tracing::warn!("tui-attach: a client connected and never said hello: {other:?}");
            return;
        }
    };
    if hello.version != proto::VERSION {
        let exit = Exit {
            code: 2,
            reason: format!(
                "protocol mismatch: this bough speaks v{}, the client v{} — rebuild one of them",
                proto::VERSION,
                hello.version
            ),
        };
        let _ = send_exit(&mut write_half, &exit).await;
        return;
    }
    let ack = ServerHello {
        version: proto::VERSION,
        mouse: tui.mouse(),
        keyboard_enhancement: tui.keyboard_enhancement(),
    };
    let Ok(payload) = proto::encode("hello", &ack) else {
        return;
    };
    if proto::write_frame(&mut write_half, proto::S_HELLO, &payload)
        .await
        .is_err()
    {
        return;
    }

    // Registration is the steal point; everything before it touched nothing shared.
    let (control_tx, mut control) = mpsc::unbounded_channel::<Exit>();
    let Some(my_seq) = state.register(control_tx) else {
        let _ = send_exit(
            &mut write_half,
            &Exit {
                code: 0,
                reason: "bough is shutting this listener down".to_string(),
            },
        )
        .await;
        return;
    };
    let (osc_tx, mut osc) = mpsc::unbounded_channel::<Vec<u8>>();
    tui.set_byte_sink(Some(osc_tx));

    // The reader sub-task: decode frames into events; forward the stream's death as a message.
    let (msg_tx, mut msgs) = mpsc::unbounded_channel::<ClientMsg>();
    let reader = tokio::spawn(async move {
        loop {
            match proto::read_frame(&mut read_half).await {
                Ok(Some((proto::C_EVENT, payload))) => {
                    match proto::decode::<Event>("event", &payload) {
                        Ok(ev) => {
                            if msg_tx.send(ClientMsg::Event(ev)).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = msg_tx.send(ClientMsg::Gone(Some(e)));
                            return;
                        }
                    }
                }
                Ok(Some(_)) => {} // an unknown tag from a same-version client: skip, stay up
                Ok(None) => {
                    let _ = msg_tx.send(ClientMsg::Gone(None));
                    return;
                }
                Err(e) => {
                    let _ = msg_tx.send(ClientMsg::Gone(Some(e)));
                    return;
                }
            }
        }
    });

    let buf = SharedBuf::default();
    let mut term = match blit_terminal(buf.clone(), hello.cols, hello.rows) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, "tui-attach: could not build the blit terminal");
            reader.abort();
            if state.end(my_seq) {
                tui.set_byte_sink(None);
            }
            return;
        }
    };
    tui.resize(hello.cols, hello.rows);
    let mut frames = tui.frames();
    frames.mark_changed(); // paint the current screen without waiting for the next draw
                           // NO `Terminal::clear()` here or anywhere in this task: it snapshots the cursor through
                           // `crossterm::cursor::position()`, which talks to THIS PROCESS's /dev/tty — a resident in a
                           // background process group is stopped by SIGTTOU for that. A fresh blit terminal diffs
                           // against blank buffers, so the first draw is the full screen; the client's own alt screen
                           // starts blank, so nothing needs erasing on its side either.

    loop {
        tokio::select! {
            biased;
            exit = control.recv() => {
                let exit = exit.unwrap_or(Exit { code: 0, reason: "bough exited".to_string() });
                let _ = send_exit(&mut write_half, &exit).await;
                break;
            }
            changed = frames.changed() => {
                if changed.is_err() {
                    // The shell is gone; the tree is coming down.
                    let _ = send_exit(&mut write_half, &Exit { code: 0, reason: "bough exited".to_string() }).await;
                    break;
                }
                let frame = tui.last_frame();
                if let Err(e) = blit(&mut term, &frame) {
                    tracing::warn!(error = %e, "tui-attach: blit failed");
                    break;
                }
                let bytes = buf.drain();
                // A draw with NO changed cells still writes the trailing style reset and cursor
                // hide; four of those a second is noise a client should not have to swallow.
                if !bytes.is_empty()
                    && bytes != IDLE_FRAME
                    && proto::write_frame(&mut write_half, proto::S_BYTES, &bytes).await.is_err()
                {
                    break;
                }
            }
            Some(bytes) = osc.recv() => {
                if proto::write_frame(&mut write_half, proto::S_BYTES, &bytes).await.is_err() {
                    break;
                }
            }
            msg = msgs.recv() => {
                match msg {
                    Some(ClientMsg::Event(ev)) => {
                        if let Event::Resize(cols, rows) = ev {
                            tui.resize(cols, rows);
                            match blit_terminal(buf.clone(), cols, rows) {
                                Ok(t) => {
                                    // A fresh terminal: blank buffers, so the next blit repaints
                                    // everything. The explicit erase is for the CLIENT's screen,
                                    // whose old cells outside the new layout nobody else owns.
                                    term = t;
                                    if proto::write_frame(
                                        &mut write_half,
                                        proto::S_BYTES,
                                        b"\x1b[2J\x1b[H",
                                    ).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "tui-attach: resize failed");
                                    break;
                                }
                            }
                        }
                        bough_plugin_tui_shell::run::on_event(&tui, ev).await;
                        tui.redraw();
                    }
                    Some(ClientMsg::Gone(err)) => {
                        if let Some(e) = err {
                            tracing::info!(error = %e, "tui-attach: the client went away");
                        }
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    reader.abort();
    if state.end(my_seq) {
        tui.set_byte_sink(None);
    }
}

/// What `Terminal::draw` writes when not one cell changed: the trailing style reset and the
/// cursor hide, nothing else. Sent to nobody's benefit, so the session skips it.
const IDLE_FRAME: &[u8] = b"\x1b[39m\x1b[49m\x1b[59m\x1b[0m\x1b[?25l";

/// Write one EXIT frame; the client restores its terminal and prints the reason.
async fn send_exit(
    w: &mut (impl tokio::io::AsyncWrite + Unpin),
    exit: &Exit,
) -> Result<(), proto::ProtoError> {
    let payload = proto::encode("exit", exit)?;
    proto::write_frame(w, proto::S_EXIT, &payload).await?;
    w.shutdown().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn frame(cols: u16, rows: u16, text: &str) -> Buffer {
        let mut b = Buffer::empty(Rect::new(0, 0, cols, rows));
        b.set_string(0, 0, text, ratatui::style::Style::default());
        b
    }

    /// The diff property the whole transport leans on: a repeated frame costs no cells.
    #[test]
    fn a_repeated_frame_sends_no_cells() {
        let buf = SharedBuf::default();
        let mut term = blit_terminal(buf.clone(), 20, 4).expect("terminal");
        let src = frame(20, 4, "hello attach");
        blit(&mut term, &src).expect("first");
        let first = String::from_utf8_lossy(&buf.drain()).to_string();
        // The space between the words is an UNCHANGED cell (both buffers start blank), so the
        // diff sends the two words with a cursor move between them, never the whole sentence.
        assert!(
            first.contains("hello") && first.contains("attach"),
            "the first blit carries the content: {first:?}"
        );
        blit(&mut term, &src).expect("second");
        let second = String::from_utf8_lossy(&buf.drain()).to_string();
        assert!(
            !second.contains("hello") && !second.contains("attach"),
            "an unchanged frame must not re-send its cells: {second:?}"
        );
    }

    /// …and a changed cell is sent while the rest is not.
    #[test]
    fn only_the_changed_cells_cross() {
        let buf = SharedBuf::default();
        let mut term = blit_terminal(buf.clone(), 20, 4).expect("terminal");
        blit(&mut term, &frame(20, 4, "aaaa bbbb")).expect("first");
        buf.drain();
        blit(&mut term, &frame(20, 4, "aaaa cccc")).expect("second");
        let diff = String::from_utf8_lossy(&buf.drain()).to_string();
        assert!(diff.contains("cccc"), "{diff:?}");
        assert!(
            !diff.contains("aaaa"),
            "unchanged cells must not re-send: {diff:?}"
        );
    }

    /// A source larger than the client's terminal clamps instead of panicking (the one-frame
    /// window around a resize).
    #[test]
    fn a_mismatched_size_clamps_to_the_overlap() {
        let buf = SharedBuf::default();
        let mut term = blit_terminal(buf.clone(), 10, 2).expect("terminal");
        blit(&mut term, &frame(40, 10, "wider than the client")).expect("clamped");
    }

    /// The registry's steal: the second register detaches the first with a named reason, and a
    /// stale session's `end` cannot take the live registration down.
    #[test]
    fn a_second_register_steals_and_a_stale_end_is_a_no_op() {
        let state = AttachState::default();
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let seq1 = state.register(tx1).expect("open");
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let seq2 = state.register(tx2).expect("open");
        let stolen = rx1.try_recv().expect("the first client is told");
        assert!(stolen.reason.contains("another bough"), "{}", stolen.reason);
        assert!(
            !state.end(seq1),
            "a stale end must not touch the live session"
        );
        assert!(state.attached());
        assert!(state.end(seq2), "the live end owns the cleanup");
        assert!(!state.attached());
    }

    /// A closed registry refuses new registrations and detaches the current client.
    #[test]
    fn close_detaches_and_refuses_later_registrations() {
        let state = AttachState::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        state.register(tx).expect("open");
        state.close("bough exited");
        assert_eq!(rx.try_recv().expect("told").reason, "bough exited");
        let (tx2, _rx2) = mpsc::unbounded_channel();
        assert!(state.register(tx2).is_none(), "closed refuses registration");
    }
}
