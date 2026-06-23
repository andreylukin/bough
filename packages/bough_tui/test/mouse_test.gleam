//// Pure-function tests for the conversation pane's mouse + highlight behaviour.
//// Everything here drives `app.update` with synthesized etch events and asserts
//// on the resulting `Model` / `app.render` output — no terminal, no PTY.
////
//// Geometry note: every coordinate below depends on the fixture's fixed size
//// (80x24) and a single `You("hello world")` entry, which lays out as:
////
////   row 1  "▌ you"        (transcript line 0)
////   row 2  ""             (line 1)
////   row 3  "hello world"  (line 2)   <- the text we select
////   row 4  ""             (line 3)
////
//// Text is drawn at screen column 2, so column N maps to char index N-2.

import bough_tui/app.{type Model, EtchEvent}
import etch/command
import etch/event.{Down, Drag, Left, Mouse, MouseEvent, ScrollUp, Up}
import etch/style
import gleam/int
import gleam/list
import gleam/option.{None, Some}

const no_mods = event.Modifiers(
  alt: False,
  control: False,
  shift: False,
  super: False,
  hyper: False,
  meta: False,
)

/// A mouse event of `kind` at screen `col`/`row`, wrapped as an app `Msg`.
fn mouse(kind: event.MouseEventKind, col: Int, row: Int) -> app.Msg {
  EtchEvent(Mouse(MouseEvent(kind: kind, column: col, row: row, modifiers: no_mods)))
}

/// 80x24 chat with one `You("hello world")` line and no startup banner.
/// `note` MUST be cleared — `init` may set it, which prepends a banner and
/// shifts every line index below.
fn fixture() -> Model {
  let #(m, _) = app.init()
  let m = app.set_size(m, #(80, 24))
  app.Model(..m, chat: [app.You("hello world")], note: None)
}

fn send(m: Model, msg: app.Msg) -> Model {
  let #(m, _effects) = app.update(m, msg)
  m
}

// --- mouse selection ------------------------------------------------------

pub fn press_in_conversation_begins_selection_test() {
  let m = fixture() |> send(mouse(Down(Left), 2, 3))
  // Anchor and head both sit on the pressed cell: line 2, column 2.
  let assert Some(app.Region(2, 2, 2, 2)) = m.mouse_sel
}

pub fn drag_selects_and_copies_text_test() {
  let m =
    fixture()
    |> send(mouse(Down(Left), 2, 3))
    |> send(mouse(Drag(Left), 6, 3))
    |> send(mouse(Up(Left), 6, 3))

  // Down→Drag→Up over columns 2..6 of "hello world" selects "hello".
  let assert Some(region) = m.mouse_sel
  assert app.selection_text(m, region) == "hello"
  // The user-visible copy receipt, observable without exposing internals.
  assert m.status == "copied 5 chars"
}

pub fn no_drag_is_a_click_and_clears_selection_test() {
  // Press and release on the same cell: a click, not a selection. The clicked
  // line carries no handler, so nothing is copied and the selection is cleared.
  let m =
    fixture()
    |> send(mouse(Down(Left), 2, 3))
    |> send(mouse(Up(Left), 2, 3))

  assert m.mouse_sel == None
  assert m.status != "copied 5 chars"
}

pub fn click_below_conversation_focuses_input_test() {
  // Row 22 is past the conversation box (height 20): a click there focuses the
  // input. Start unfocused so we observe the transition.
  let m = app.Model(..fixture(), focused: False)
  let m = send(m, mouse(Down(Left), 5, 22))
  assert m.focused == True
}

// --- highlight rendering --------------------------------------------------

pub fn selection_renders_blue_highlight_test() {
  // After the drag the region stays highlighted; `render` repaints those cells
  // with a blue background. The overlay prints exactly the selected slice
  // ("hello"), which the base transcript line ("hello world") never does — so a
  // `Print("hello")` command is unique to the highlight overlay.
  let m =
    fixture()
    |> send(mouse(Down(Left), 2, 3))
    |> send(mouse(Drag(Left), 6, 3))
    |> send(mouse(Up(Left), 6, 3))

  let cmds = app.render(m)
  assert list.contains(cmds, command.Print("hello"))
  assert list.contains(
    cmds,
    command.SetStyle(style.Style(bg: style.Blue, fg: style.White, attributes: [])),
  )
}

// --- scrolling while a selection is active --------------------------------

/// A transcript tall enough that the conversation actually scrolls (the visible
/// budget at 80x24 is 18 rows; 10 `You` entries lay out to ~40 lines).
fn tall_fixture() -> Model {
  let #(m, _) = app.init()
  let m = app.set_size(m, #(80, 24))
  let entries =
    [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    |> list.map(fn(i) { app.You("line " <> int.to_string(i)) })
  app.Model(..m, chat: entries, note: None)
}

pub fn wheel_keeps_and_extends_active_selection_test() {
  // Make a selection, then scroll the wheel up to reach more content. The
  // selection must survive (regression: it used to be cleared on every wheel
  // tick) and its head should extend toward the newly exposed content.
  let m =
    tall_fixture()
    |> send(mouse(Down(Left), 2, 5))
    |> send(mouse(Drag(Left), 8, 5))
    |> send(mouse(Up(Left), 8, 5))
  let assert Some(app.Region(anchor_line, anchor_col, _, _)) = m.mouse_sel

  let scrolled = send(m, EtchEvent(Mouse(MouseEvent(
    kind: ScrollUp, column: 10, row: 5, modifiers: no_mods,
  ))))

  // Selection preserved, anchor unchanged, and we actually scrolled.
  let assert Some(app.Region(a_line, a_col, _, _)) = scrolled.mouse_sel
  assert a_line == anchor_line
  assert a_col == anchor_col
  assert scrolled.scroll > m.scroll
}

pub fn wheel_without_selection_just_scrolls_test() {
  let m = tall_fixture()
  let scrolled = send(m, EtchEvent(Mouse(MouseEvent(
    kind: ScrollUp, column: 10, row: 5, modifiers: no_mods,
  ))))
  assert scrolled.mouse_sel == None
  assert scrolled.scroll > m.scroll
}

pub fn upward_autoscroll_ramps_over_held_ticks_test() {
  // Drag past the top edge (row 0) to start autoscrolling up. The drag can only
  // set a 1-line/tick speed there (no screen room above row 0), so without the
  // time ramp it would stay at 1 forever — the reported "can't scroll up while
  // highlighting" bug. Held ticks must accelerate it.
  let dragging =
    tall_fixture()
    |> send(mouse(Down(Left), 2, 5))
    |> send(mouse(Drag(Left), 5, 0))
  assert dragging.autoscroll == 1

  let t1 = send(dragging, app.AutoScroll)
  let t2 = send(t1, app.AutoScroll)
  // Speed accelerates and direction (up / positive) is preserved.
  assert t1.autoscroll == 3
  assert t2.autoscroll == 5
  // Selection stays live through the ramp.
  assert t2.mouse_sel != None
}

pub fn autoscroll_ramp_survives_jittery_drag_test() {
  // A trackpad held past an edge emits continuous motion (Drag) events. Those
  // used to reset the speed to the positional base (1) on every report, so the
  // ramp never accumulated — the "still painfully slow" bug. A Drag past the
  // same edge between ticks must NOT reset the ramped speed.
  let dragging =
    tall_fixture()
    |> send(mouse(Down(Left), 2, 5))
    |> send(mouse(Drag(Left), 5, 0))
  let ramped = send(dragging, app.AutoScroll)
  let speed = ramped.autoscroll
  assert speed > 1

  // Jitter: another Drag past the top edge arrives before the next tick.
  let jittered = send(ramped, mouse(Drag(Left), 5, 0))
  assert jittered.autoscroll == speed

  // And the ramp keeps climbing from there.
  let next = send(jittered, app.AutoScroll)
  assert next.autoscroll > speed
}
