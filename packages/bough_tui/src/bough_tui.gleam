//// bough TUI client.
////
//// Connects to a bough server over HTTP + SSE. Default `bough` (no subcommand)
//// starts a server and attaches this client, opencode-style (SPEC.md §8).
//// Layout: chat pane + live network side pane, with the session tree as an
//// overlay (SPEC.md §9). TUI library (shore/etch/plushie) chosen during the
//// slice and isolated behind `bough_tui/app`.

import gleam/io

pub fn main() -> Nil {
  io.println("bough TUI — not yet implemented; see SPEC.md §9")
}
