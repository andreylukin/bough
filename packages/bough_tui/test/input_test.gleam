//// Tests for `app.split_pending_escape`, which holds an unterminated trailing
//// escape sequence across terminal reads so a split mouse/key sequence is never
//// parsed as text. `esc` is the CSI/SS3 introducer (0x1B).

import bough_tui/app
import gleam/list
import gleam/string

const esc = "\u{001b}"

fn g(s: String) -> List(String) {
  string.to_graphemes(s)
}

// Plain text carries nothing across reads.
pub fn plain_text_test() {
  assert app.split_pending_escape(g("hello")) == #(g("hello"), [])
}

// A complete SGR mouse report parses entirely now.
pub fn complete_sgr_mouse_test() {
  let seq = esc <> "[<64;10;20M"
  assert app.split_pending_escape(g(seq)) == #(g(seq), [])
}

// A report cut mid-params is held whole for the next read.
pub fn split_sgr_mouse_test() {
  let partial = esc <> "[<64;10;"
  assert app.split_pending_escape(g(partial)) == #([], g(partial))
}

// Text preceding a cut sequence is safe; only the sequence tail is held.
pub fn text_then_split_test() {
  let buffer = "ab" <> esc <> "[<64;"
  assert app.split_pending_escape(g(buffer)) == #(g("ab"), g(esc <> "[<64;"))
}

// Re-feeding the held tail with the rest of the read completes the sequence.
pub fn reassembly_test() {
  let #(_safe, pending) = app.split_pending_escape(g(esc <> "[<64;"))
  let combined = list.append(pending, g("10;20M"))
  let whole = esc <> "[<64;10;20M"
  assert app.split_pending_escape(combined) == #(g(whole), [])
}

// A lone trailing ESC is the Esc key — parsed now, not held (Esc stays snappy).
pub fn lone_esc_test() {
  assert app.split_pending_escape(g(esc)) == #(g(esc), [])
}

// An earlier complete sequence is safe even when a later one is cut.
pub fn complete_then_split_test() {
  let buffer = esc <> "[A" <> esc <> "[<64;"
  assert app.split_pending_escape(g(buffer))
    == #(g(esc <> "[A"), g(esc <> "[<64;"))
}

// SS3 (`ESC O`) needs its one following byte before it can be parsed.
pub fn ss3_incomplete_test() {
  let partial = esc <> "O"
  assert app.split_pending_escape(g(partial)) == #([], g(partial))
}

pub fn ss3_complete_test() {
  let seq = esc <> "OA"
  assert app.split_pending_escape(g(seq)) == #(g(seq), [])
}
