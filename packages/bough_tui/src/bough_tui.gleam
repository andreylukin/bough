//// bough TUI client.
////
//// Connects to a bough server over HTTP + SSE. Default `bough` (no subcommand)
//// starts a server and attaches this client, opencode-style (SPEC.md §8).
//// Layout: chat pane + live network side pane, with the session tree as an
//// overlay (SPEC.md §9). TUI rendering (shore/etch/plushie) is added next,
//// behind `bough_tui/app`. For now `main` proves it can reach the server.

import bough_tui/client
import gleam/io

const default_server = "http://127.0.0.1:4096"

pub fn main() -> Nil {
  io.println("bough TUI — connecting to " <> default_server)
  case client.health(default_server) {
    Ok(body) -> io.println("connected: " <> body)
    Error(_) ->
      io.println("could not reach bough server — start it with `make serve`")
  }
}
