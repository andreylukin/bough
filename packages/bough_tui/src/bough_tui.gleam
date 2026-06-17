//// bough TUI client entry point.
////
//// Starts a shore TUI that talks to a bough server over HTTP (SPEC.md §8, §9).
//// Set BOUGH_SERVER to point at a non-default server; the session's project
//// defaults to $PWD.

import bough_tui/app
import gleam/erlang/process
import shore

pub fn main() -> Nil {
  let exit = process.new_subject()
  let assert Ok(_actor) =
    shore.spec(
      init: app.init,
      update: app.update,
      view: app.view,
      exit: exit,
      keybinds: shore.default_keybinds(),
      redraw: shore.on_timer(100),
    )
    |> shore.start
  process.receive_forever(exit)
}
