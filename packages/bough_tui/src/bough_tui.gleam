//// bough TUI client entry point (etch backend).
////
//// etch is a terminal backend, so we own the lifecycle: enter raw mode, switch
//// to the alternate screen, enable mouse capture, then run a receive loop. A
//// dedicated process blocks on `input.read()` and forwards events; HTTP polls
//// and the spinner tick arrive on the same subject as `update` effects.

import bough_tui/app
import etch/command
import etch/event
import etch/erlang/input
import etch/erlang/tty
import etch/stdout
import etch/terminal
import gleam/erlang/process
import gleam/list
import gleam/option.{Some}
import gleam/string

@external(erlang, "erlang", "halt")
fn halt(n: Int) -> Nil

// etch's own input loop (`tty.start_input_loop`) reparses each terminal read
// from scratch, so an escape sequence split across reads leaks its tail as text
// (mouse reports while scrolling, in particular). We run our own loop that
// carries the unterminated tail across reads, but reuse etch's event server:
// `input_ffi:start_link` owns the queue + SIGWINCH→resize handler, and
// `input.read()` drains it as before. So we bring up those pieces directly and
// skip `start_input_loop`.
@external(erlang, "tty_state", "init")
fn init_tty_state() -> Nil

@external(erlang, "input_ffi", "start_link")
fn start_event_server() -> Nil

@external(erlang, "input_ffi", "push")
fn push_event(event: Result(event.Event, event.EventError)) -> Nil

@external(erlang, "io", "get_chars")
fn get_chars(prompt: String, count: Int) -> String

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
    // Negotiate the kitty keyboard protocol so terminals (e.g. Ghostty) report
    // modifiers like Super/Cmd on functional keys such as the arrows.
    command.PushKeyboardEnhancementFlags([event.DisambiguateEscapeCode]),
    command.HideCursor,
    command.Clear(terminal.All),
  ])
  init_tty_state()
  start_event_server()
  process.spawn(fn() { read_loop([]) })

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
  let model = step(self, model, process.receive_forever(self))
  case app.is_quit(model) {
    True -> teardown()
    False -> {
      // Apply any messages already queued (e.g. a burst of wheel events) before
      // painting, so one render reflects the whole batch instead of one render
      // per event — that per-event redraw is what made fast scrolling hang.
      let model = drain(self, model)
      case app.is_quit(model) {
        True -> teardown()
        False -> {
          render(model)
          loop(self, model)
        }
      }
    }
  }
}

/// Apply one message: update the model and fire its effects.
fn step(self: process.Subject(app.Msg), model: app.Model, msg: app.Msg) -> app.Model {
  let #(model, effects) = app.update(model, msg)
  run_effects(self, effects)
  model
}

/// Drain messages already in the mailbox without blocking, stopping early on a
/// quit so the caller can tear down promptly.
fn drain(self: process.Subject(app.Msg), model: app.Model) -> app.Model {
  case process.receive(self, 0) {
    Ok(msg) -> {
      let model = step(self, model, msg)
      case app.is_quit(model) {
        True -> model
        False -> drain(self, model)
      }
    }
    Error(_) -> model
  }
}

fn input_reader(self: process.Subject(app.Msg)) -> Nil {
  case input.read() {
    Some(Ok(ev)) -> process.send(self, app.EtchEvent(ev))
    _ -> Nil
  }
  input_reader(self)
}

/// Read terminal bytes and parse them into events, carrying any unterminated
/// trailing escape sequence into the next read so a split mouse/key sequence is
/// never parsed as text. Parsed events go onto etch's queue (read via
/// `input.read()`), alongside the resize events its signal handler pushes.
fn read_loop(pending: List(String)) -> Nil {
  let buffer = list.append(pending, string.to_graphemes(get_chars("", 128)))
  let #(ready, pending) = app.split_pending_escape(buffer)
  list.each(event.parse_events(ready, "", [], False), push_event)
  read_loop(pending)
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
    command.PopKeyboardEnhancementFlags,
    command.DisableMouseCapture,
    command.ShowCursor,
    command.LeaveAlternateScreen,
  ])
  let _ = tty.exit_raw()
  halt(0)
}
