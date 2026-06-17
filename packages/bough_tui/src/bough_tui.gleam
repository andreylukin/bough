//// bough TUI client entry point (etch backend).
////
//// etch is a terminal backend, so we own the lifecycle: enter raw mode, switch
//// to the alternate screen, enable mouse capture, then run a receive loop. A
//// dedicated process blocks on `input.read()` and forwards events; HTTP polls
//// and the spinner tick arrive on the same subject as `update` effects.

import bough_tui/app
import etch/command
import etch/erlang/input
import etch/erlang/tty
import etch/stdout
import etch/terminal
import gleam/erlang/process
import gleam/list
import gleam/option.{Some}

@external(erlang, "erlang", "halt")
fn halt(n: Int) -> Nil

pub fn main() -> Nil {
  // The terminal setup (shell:start_interactive for raw mode) must run off the
  // `-eval` boot process — it throws there, but works from a spawned process
  // (this is how shore drives it too). Keep `main` alive while the worker runs.
  let parent = process.new_subject()
  process.spawn(fn() { run() })
  process.receive_forever(parent)
}

fn run() -> Nil {
  let _ = tty.enter_raw()
  stdout.execute([
    command.EnterAlternateScreen,
    command.EnableMouseCapture,
    command.HideCursor,
    command.Clear(terminal.All),
  ])
  input.init_event_server()

  let self = process.new_subject()
  let #(model, effects) = app.init()
  let model = case tty.window_size() {
    Ok(size) -> app.set_size(model, size)
    Error(_) -> model
  }

  process.spawn(fn() { input_reader(self) })
  run_effects(self, effects)
  render(model)
  loop(self, model)
}

fn loop(self: process.Subject(app.Msg), model: app.Model) -> Nil {
  let msg = process.receive_forever(self)
  let #(model, effects) = app.update(model, msg)
  case app.is_quit(model) {
    True -> teardown()
    False -> {
      render(model)
      run_effects(self, effects)
      loop(self, model)
    }
  }
}

fn input_reader(self: process.Subject(app.Msg)) -> Nil {
  case input.read() {
    Some(Ok(ev)) -> process.send(self, app.EtchEvent(ev))
    _ -> Nil
  }
  input_reader(self)
}

fn run_effects(
  self: process.Subject(app.Msg),
  effects: List(fn() -> app.Msg),
) -> Nil {
  list.each(effects, fn(eff) {
    process.spawn(fn() { process.send(self, eff()) })
  })
}

fn render(model: app.Model) -> Nil {
  stdout.execute(app.render(model))
}

fn teardown() -> Nil {
  stdout.execute([
    command.DisableMouseCapture,
    command.ShowCursor,
    command.LeaveAlternateScreen,
  ])
  let _ = tty.exit_raw()
  halt(0)
}
