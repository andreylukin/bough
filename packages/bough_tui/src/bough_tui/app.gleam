//// The bough TUI on the etch backend.
////
//// etch is a terminal backend (raw mode, events, styled output), not a
//// framework, so this module owns the model/update/render directly. The event
//// loop and terminal lifecycle live in `bough_tui`. We keep the shore-era
//// effect convention: `update` returns `#(Model, List(fn() -> Msg))` and the
//// loop spawns each effect, sending its result back.
////
//// etch gives us native mouse (wheel + click) and resize events, so the
//// conversation scrolls with the wheel, tool output expands on click, and we
//// manage input focus ourselves (Esc toggles a typing/command mode) instead of
//// shore's Tab-focus dance.

import bough_tui/client.{type Step, Text, ToolCall, ToolResult}
import etch/command.{type Command}
import etch/event.{type Event}
import etch/style.{type Attribute, type Color}
import etch/terminal
import envoy
import gleam/dynamic/decode
import gleam/erlang/process
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/set.{type Set}
import gleam/string
import simplifile

const default_server = "http://127.0.0.1:4096"

const spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]

const result_lines = 6

const max_suggestions = 8

const max_files = 2000

pub type Entry {
  You(String)
  Bough(List(Step))
  Failed(String)
}

pub type Model {
  Model(
    server: String,
    project: String,
    session: Option(String),
    input: String,
    // Newest first; reversed for display.
    chat: List(Entry),
    status: String,
    pending: Bool,
    frame: Int,
    running_steps: List(Step),
    note: Option(String),
    files: List(String),
    suggestions: List(String),
    // Rows scrolled up from the bottom; 0 follows the latest output.
    scroll: Int,
    // Terminal size #(columns, rows).
    size: #(Int, Int),
    // True while typing into the input; False = command/scroll mode.
    focused: Bool,
    // Tool-result indices (appearance order) shown in full.
    expanded: Set(Int),
    // Expand every tool result regardless of `expanded`.
    expand_all: Bool,
    // Set once the user asks to quit.
    quit: Bool,
  )
}

pub type Msg {
  EtchEvent(Event)
  SessionCreated(Result(String, String))
  FilesScanned(List(String))
  Started(Result(Nil, String))
  Polled(Result(client.RunState, String))
  Tick
  // Internal messages produced by translating events.
  InputChanged(String)
  Submit
  ScrollBy(Int)
  ToggleResult(Int)
  ToggleAll
  SetFocus(Bool)
  Quit
}

pub fn init() -> #(Model, List(fn() -> Msg)) {
  let server = envoy.get("BOUGH_SERVER") |> result.unwrap(default_server)
  let project =
    envoy.get("BOUGH_PROJECT")
    |> result.or(envoy.get("PWD"))
    |> result.unwrap(".")
  let model =
    Model(
      server: server,
      project: project,
      session: None,
      input: "",
      chat: [],
      status: "connecting to " <> server <> " …",
      pending: False,
      frame: 0,
      running_steps: [],
      note: unsandboxable_note(project),
      files: [],
      suggestions: [],
      scroll: 0,
      size: #(80, 24),
      focused: True,
      expanded: set.new(),
      expand_all: False,
      quit: False,
    )
  #(model, [
    fn() { SessionCreated(client.create_session(server, project)) },
    fn() { FilesScanned(list_project_files(project)) },
  ])
}

pub fn set_size(model: Model, size: #(Int, Int)) -> Model {
  Model(..model, size: size)
}

pub fn is_quit(model: Model) -> Bool {
  model.quit
}

// --- Update ---------------------------------------------------------------

pub fn update(model: Model, msg: Msg) -> #(Model, List(fn() -> Msg)) {
  case msg {
    EtchEvent(ev) -> on_event(model, ev)

    SessionCreated(Ok(id)) -> #(
      Model(..model, session: Some(id), status: "ready · session " <> id),
      [],
    )
    SessionCreated(Error(e)) -> #(Model(..model, status: "error: " <> e), [])

    FilesScanned(files) -> #(Model(..model, files: files), [])

    InputChanged(value) -> #(
      Model(
        ..model,
        input: value,
        suggestions: suggestions_for(value, model.files),
        scroll: 0,
      ),
      [],
    )

    Submit -> submit(model)

    Started(Ok(_)) ->
      case model.session {
        Some(id) -> #(model, [poll(model.server, id)])
        None -> #(model, [])
      }
    Started(Error(e)) -> #(
      Model(
        ..model,
        status: "error",
        pending: False,
        chat: [Failed(e), ..model.chat],
      ),
      [],
    )

    Polled(Ok(run)) -> polled(model, run)
    Polled(Error(_)) ->
      case model.session, model.pending {
        Some(id), True -> #(model, [poll(model.server, id)])
        _, _ -> #(model, [])
      }

    Tick ->
      case model.pending {
        True -> #(Model(..model, frame: model.frame + 1), [tick])
        False -> #(model, [])
      }

    ScrollBy(n) -> #(scroll_by(model, n), [])

    ToggleResult(i) -> {
      let expanded = case set.contains(model.expanded, i) {
        True -> set.delete(model.expanded, i)
        False -> set.insert(model.expanded, i)
      }
      #(Model(..model, expanded: expanded), [])
    }

    ToggleAll -> #(Model(..model, expand_all: !model.expand_all), [])

    SetFocus(f) -> #(Model(..model, focused: f), [])

    Quit -> #(Model(..model, quit: True), [])
  }
}

fn submit(model: Model) -> #(Model, List(fn() -> Msg)) {
  // An active `@` completion: Enter accepts it instead of sending.
  case model.suggestions {
    [top, ..] -> #(
      Model(..model, input: accept_completion(model.input, top), suggestions: []),
      [],
    )
    [] ->
      case model.session, string.trim(model.input) {
        Some(id), text if text != "" -> {
          let model =
            Model(
              ..model,
              input: "",
              suggestions: [],
              scroll: 0,
              status: "thinking …",
              pending: True,
              frame: 0,
              running_steps: [],
              chat: [You(text), ..model.chat],
            )
          let server = model.server
          #(model, [fn() { Started(client.start_run(server, id, text)) }, tick])
        }
        _, _ -> #(model, [])
      }
  }
}

fn polled(model: Model, run: client.RunState) -> #(Model, List(fn() -> Msg)) {
  case run.status {
    "done" -> #(
      Model(
        ..model,
        status: "ready",
        pending: False,
        running_steps: [],
        chat: [Bough(run.steps), ..model.chat],
      ),
      [],
    )
    "error" -> #(
      Model(
        ..model,
        status: "error",
        pending: False,
        running_steps: [],
        chat: [Failed(error_text(run.text)), ..model.chat],
      ),
      [],
    )
    _ ->
      case model.session {
        Some(id) -> #(Model(..model, running_steps: run.steps), [
          poll(model.server, id),
        ])
        None -> #(Model(..model, running_steps: run.steps), [])
      }
  }
}

fn error_text(text: String) -> String {
  case text {
    "" -> "agent run failed"
    t -> t
  }
}

fn poll(server: String, id: String) -> fn() -> Msg {
  fn() {
    process.sleep(400)
    Polled(client.get_run(server, id))
  }
}

fn tick() -> Msg {
  process.sleep(120)
  Tick
}

fn scroll_by(model: Model, n: Int) -> Model {
  let budget = conv_inner_h(model)
  let max_scroll =
    int.max(list.length(transcript(model)) - budget + 2, 0)
  Model(..model, scroll: int.clamp(model.scroll + n, 0, max_scroll))
}

// --- Event translation ----------------------------------------------------

fn on_event(model: Model, ev: Event) -> #(Model, List(fn() -> Msg)) {
  case ev {
    event.Resize(c, r) -> #(Model(..model, size: #(c, r)), [])
    event.FocusGained -> #(model, [])
    event.FocusLost -> #(model, [])
    event.Key(ke) ->
      case ke.kind {
        event.Release -> #(model, [])
        _ -> on_key(model, ke)
      }
    event.Mouse(me) -> on_mouse(model, me)
  }
}

fn on_key(model: Model, ke: event.KeyEvent) -> #(Model, List(fn() -> Msg)) {
  // etch's legacy parser delivers Ctrl+key as the raw control codepoint with no
  // control modifier, so match those directly (and the kitty path too).
  case ke.code {
    // Ctrl+X (0x18) / Ctrl+C (0x03) always quit.
    event.Char("\u{0018}") | event.Char("\u{0003}") -> update(model, Quit)
    event.Char(c) if ke.modifiers.control ->
      case string.lowercase(c) {
        "c" | "x" -> update(model, Quit)
        _ -> #(model, [])
      }
    _ ->
      case model.focused {
        True -> on_key_typing(model, ke)
        False -> on_key_command(model, ke)
      }
  }
}

fn on_key_typing(
  model: Model,
  ke: event.KeyEvent,
) -> #(Model, List(fn() -> Msg)) {
  case ke.code {
    event.Enter -> update(model, Submit)
    event.Esc -> update(model, SetFocus(False))
    event.Backspace ->
      update(model, InputChanged(string.drop_end(model.input, 1)))
    event.Char(c) ->
      case printable(c) {
        True -> update(model, InputChanged(model.input <> c))
        False -> #(model, [])
      }
    _ -> #(model, [])
  }
}

/// Reject control codepoints so stray Ctrl+key bytes never land in the input.
fn printable(c: String) -> Bool {
  case string.to_utf_codepoints(c) {
    [cp] -> string.utf_codepoint_to_int(cp) >= 0x20
    _ -> True
  }
}

fn on_key_command(
  model: Model,
  ke: event.KeyEvent,
) -> #(Model, List(fn() -> Msg)) {
  case ke.code {
    event.Char("i") | event.Enter -> update(model, SetFocus(True))
    event.Esc -> update(model, SetFocus(True))
    event.UpArrow | event.Char("k") -> update(model, ScrollBy(1))
    event.DownArrow | event.Char("j") -> update(model, ScrollBy(-1))
    event.PageUp -> update(model, ScrollBy(10))
    event.PageDown -> update(model, ScrollBy(-10))
    event.Char("o") -> update(model, ToggleAll)
    event.Char("g") -> #(scroll_by(model, 100_000), [])
    _ -> #(model, [])
  }
}

fn on_mouse(
  model: Model,
  me: event.MouseEvent,
) -> #(Model, List(fn() -> Msg)) {
  case me.kind {
    event.ScrollUp -> update(model, ScrollBy(3))
    event.ScrollDown -> update(model, ScrollBy(-3))
    event.Down(event.Left) -> on_click(model, me.column, me.row)
    _ -> #(model, [])
  }
}

fn on_click(model: Model, _col: Int, row: Int) -> #(Model, List(fn() -> Msg)) {
  // A click below the conversation (input box) focuses it; a click on a
  // tool-result line toggles that result.
  let #(_cols, _rows, _conv_w, conv_h) = dims(model)
  case row >= conv_h {
    True -> update(model, SetFocus(True))
    False ->
      case list.key_find(clickable_rows(model), row) {
        Ok(msg) -> update(model, msg)
        Error(_) -> #(model, [])
      }
  }
}

// --- Layout dimensions ----------------------------------------------------

fn dims(model: Model) -> #(Int, Int, Int, Int) {
  let #(cols, rows) = model.size
  let net_w = int.max(cols * 32 / 100, 24)
  let conv_w = int.max(cols - net_w, 24)
  // conversation box height: terminal minus input box (3) and status (1).
  let conv_h = int.max(rows - 4, 3)
  #(cols, rows, conv_w, conv_h)
}

fn conv_text_width(model: Model) -> Int {
  let #(_cols, _rows, conv_w, _conv_h) = dims(model)
  int.max(conv_w - 4, 20)
}

fn conv_inner_h(model: Model) -> Int {
  let #(_cols, _rows, _conv_w, conv_h) = dims(model)
  int.max(conv_h - 2, 1)
}

// --- Transcript (as styled, clickable lines) ------------------------------

pub type CLine {
  CLine(text: String, color: Color, attrs: List(Attribute), click: Option(Msg))
}

fn line(text: String, color: Color) -> CLine {
  CLine(text, color, [], None)
}

/// A bold line (used for turn headers and markdown headings).
fn bold(text: String, color: Color) -> CLine {
  CLine(text, color, [style.Bold], None)
}

/// A dim, low-emphasis line (tool metadata, hints, chrome).
fn dim(text: String, color: Color) -> CLine {
  CLine(text, color, [style.Dim], None)
}

/// Build the whole transcript, one CLine per visual row, threading a counter so
/// each tool result has a stable index for per-item expand/collapse.
fn transcript(model: Model) -> List(CLine) {
  let width = conv_text_width(model)
  let banner = case model.note {
    Some(text) -> list.append(wrap_styled(text, width, style.Red), [line("", style.Default)])
    None -> []
  }
  let entries = list.reverse(model.chat)
  let #(history, idx) =
    list.fold(entries, #([], 0), fn(acc, entry) {
      let #(lines, i) = acc
      let #(more, i2) = render_entry(entry, width, model, i)
      #(list.append(lines, more), i2)
    })
  let live = case model.pending {
    True -> {
      let #(more, _) = render_entry(Bough(model.running_steps), width, model, idx)
      list.append(more, [thinking_line(model.frame)])
    }
    False -> []
  }
  let main = case history, live {
    [], [] -> [
      dim(
        "type a task · Enter to send · @ to mention a file · Esc for scroll/mouse",
        style.Grey,
      ),
    ]
    _, _ -> list.append(history, live)
  }
  list.flatten([banner, main, suggestion_lines(model.suggestions)])
}

fn render_entry(
  entry: Entry,
  width: Int,
  model: Model,
  idx: Int,
) -> #(List(CLine), Int) {
  case entry {
    You(text) -> #(
      list.flatten([
        [bold("▌ you", style.Cyan)],
        wrap_styled(text, width, style.Default),
        [line("", style.Default)],
      ]),
      idx,
    )
    Failed(e) -> #(
      list.flatten([
        [bold("▌ error", style.Red)],
        wrap_styled(e, width, style.Red),
        [line("", style.Default)],
      ]),
      idx,
    )
    Bough(steps) -> {
      let #(step_lines, idx2) =
        list.fold(steps, #([], idx), fn(acc, step) {
          let #(ls, i) = acc
          let #(more, i2) = render_step(step, width, model, i)
          #(list.append(ls, more), i2)
        })
      #(
        list.flatten([[bold("▌ bough", style.Green)], step_lines, [line("", style.Default)]]),
        idx2,
      )
    }
  }
}

fn render_step(
  step: Step,
  width: Int,
  model: Model,
  idx: Int,
) -> #(List(CLine), Int) {
  case step {
    Text(text) -> #(render_markdown(text, width), idx)
    ToolCall(name, input) -> #(
      [dim("↳ " <> format_tool_call(name, input), style.Grey)],
      idx,
    )
    ToolResult(_name, output) -> #(render_result(output, width, model, idx), idx + 1)
  }
}

fn render_result(
  output: String,
  width: Int,
  model: Model,
  idx: Int,
) -> List(CLine) {
  let expanded = model.expand_all || set.contains(model.expanded, idx)
  case string.trim_end(output) {
    "" -> [rail("(no output)"), line("", style.Default)]
    trimmed -> {
      let lines = string.split(trimmed, "\n")
      let total = list.length(lines)
      let railed = fn(ls) { list.map(ls, fn(l) { rail(truncate(l, width - 2)) }) }
      case expanded {
        True ->
          list.flatten([
            railed(lines),
            [
              CLine(
                "  ▾ collapse",
                style.Cyan,
                [],
                Some(ToggleResult(idx)),
              ),
            ],
            [line("", style.Default)],
          ])
        False -> {
          let shown = railed(list.take(lines, result_lines))
          let more = case total - result_lines {
            extra if extra > 0 -> [
              CLine(
                "  ▸ +" <> int.to_string(extra) <> " lines",
                style.Cyan,
                [],
                Some(ToggleResult(idx)),
              ),
            ]
            _ -> []
          }
          list.flatten([shown, more, [line("", style.Default)]])
        }
      }
    }
  }
}

fn rail(text: String) -> CLine {
  // Grey but not Dim: secondary to prose, yet legible when expanded for reading.
  line("  │ " <> text, style.Grey)
}

/// Lightweight markdown. Hierarchy comes from weight (bold headings) and dim
/// (code/quotes), not a spread of hues — keeps prose the brightest thing.
fn render_markdown(text: String, width: Int) -> List(CLine) {
  let lines = string.split(text, "\n")
  let #(acc, _in_code) =
    list.fold(lines, #([], False), fn(state, l) {
      let #(acc, in_code) = state
      case string.starts_with(string.trim_start(l), "```") {
        // Fence lines toggle code mode and aren't rendered themselves.
        True -> #(acc, !in_code)
        False ->
          case in_code {
            True -> #(list.append(acc, [code_line(l, width)]), True)
            False -> #(list.append(acc, render_md_line(l, width)), False)
          }
      }
    })
  acc
}

fn code_line(l: String, width: Int) -> CLine {
  line("▏ " <> truncate(l, width - 2), style.Cyan)
}

fn render_md_line(l: String, width: Int) -> List(CLine) {
  let trimmed = string.trim_end(l)
  case
    strip_prefix(trimmed, "### "),
    strip_prefix(trimmed, "## "),
    strip_prefix(trimmed, "# "),
    bullet_prefix(string.trim_start(l)),
    strip_prefix(string.trim_start(l), "> ")
  {
    Ok(h), _, _, _, _ | _, Ok(h), _, _, _ | _, _, Ok(h), _, _ ->
      [bold(h, style.Default)]
    _, _, _, Ok(rest), _ -> bullet_lines(rest, width)
    _, _, _, _, Ok(q) -> [dim("▏ " <> truncate(q, width - 2), style.Grey)]
    _, _, _, _, _ ->
      case trimmed == "---" || trimmed == "***" || trimmed == "___" {
        True -> [dim(string.repeat("─", int.min(width, 40)), style.Grey)]
        False -> wrap_plain(l, width)
      }
  }
}

fn bullet_prefix(s: String) -> Result(String, Nil) {
  case strip_prefix(s, "- "), strip_prefix(s, "* "), strip_prefix(s, "+ ") {
    Ok(r), _, _ | _, Ok(r), _ | _, _, Ok(r) -> Ok(r)
    _, _, _ -> Error(Nil)
  }
}

fn bullet_lines(rest: String, width: Int) -> List(CLine) {
  wrap_text(rest, width - 2)
  |> list.index_map(fn(l, i) {
    case i {
      0 -> line("• " <> l, style.Default)
      _ -> line("  " <> l, style.Default)
    }
  })
}

fn thinking_line(frame: Int) -> CLine {
  let glyph = case list.drop(spinner, frame % 10) {
    [g, ..] -> g
    [] -> "⠋"
  }
  dim(glyph <> " thinking …", style.Grey)
}

fn suggestion_lines(suggestions: List(String)) -> List(CLine) {
  case suggestions {
    [] -> []
    _ -> {
      let header = dim("@ files · Enter completes top", style.Grey)
      let rows =
        list.index_map(suggestions, fn(path, i) {
          case i {
            0 -> line("→ " <> path, style.Cyan)
            _ -> dim("  " <> path, style.Grey)
          }
        })
      [line("", style.Default), header, ..rows]
    }
  }
}

// --- Scroll windowing -----------------------------------------------------

/// The visible conversation lines paired with their absolute screen rows, after
/// applying scroll. Shared by the renderer and mouse hit-testing.
fn visible_conversation(model: Model) -> List(#(Int, CLine)) {
  let budget = conv_inner_h(model)
  let windowed = scroll_window(transcript(model), budget, model.scroll)
  // Content starts at row 1 (row 0 is the box's top border).
  list.index_map(windowed, fn(cl, i) { #(i + 1, cl) })
}

fn clickable_rows(model: Model) -> List(#(Int, Msg)) {
  visible_conversation(model)
  |> list.filter_map(fn(pair) {
    let #(row, cl) = pair
    case cl.click {
      Some(msg) -> Ok(#(row, msg))
      None -> Error(Nil)
    }
  })
}

fn scroll_window(nodes: List(CLine), budget: Int, scroll: Int) -> List(CLine) {
  let total = list.length(nodes)
  case total <= budget {
    True -> nodes
    False -> {
      let scrolled = scroll > 0
      let inner = int.max(budget - 1 - bool_int(scrolled), 1)
      let max_scroll = total - inner
      let s = int.clamp(scroll, 0, max_scroll)
      let start = int.max(total - inner - s, 0)
      let visible = nodes |> list.drop(start) |> list.take(inner)
      let above = start
      let below = total - start - inner
      let top = case above > 0 {
        True -> [dim("⋯ " <> int.to_string(above) <> " above", style.Grey)]
        False -> []
      }
      let bottom = case below > 0 {
        True -> [dim("⋯ " <> int.to_string(below) <> " below", style.Grey)]
        False -> []
      }
      list.flatten([top, visible, bottom])
    }
  }
}

fn bool_int(b: Bool) -> Int {
  case b {
    True -> 1
    False -> 0
  }
}

// --- Render to etch commands ----------------------------------------------

pub fn render(model: Model) -> List(Command) {
  let #(cols, rows, conv_w, conv_h) = dims(model)
  let net_x = conv_w
  let net_w = cols - conv_w

  let convo =
    visible_conversation(model)
    |> list.flat_map(fn(pair) {
      let #(row, cl) = pair
      draw(2, row, truncate(cl.text, conv_w - 3), cl.color, cl.attrs)
    })

  list.flatten([
    [command.Clear(terminal.All)],
    box(0, 0, conv_w, conv_h, "conversation", style.Blue),
    convo,
    box(net_x, 0, net_w, conv_h, "network", style.Magenta),
    network_panel(model, net_x + 2, net_w - 3),
    box(0, conv_h, cols, 3, input_title(model), style.Cyan),
    input_panel(model, conv_h, cols),
    status_line(model, rows - 1, cols),
    cursor(model, conv_h, cols),
  ])
}

fn input_title(model: Model) -> String {
  case model.focused {
    True -> "message"
    False -> "message — i/Enter to type"
  }
}

fn input_panel(model: Model, conv_h: Int, cols: Int) -> List(Command) {
  let avail = cols - 5
  let shown = case string.length(model.input) > avail {
    True -> string.slice(model.input, string.length(model.input) - avail, avail)
    False -> model.input
  }
  let mark_color = case model.focused {
    True -> style.Cyan
    False -> style.Grey
  }
  list.flatten([
    draw(2, conv_h + 1, "›", mark_color, []),
    put(4, conv_h + 1, shown, style.Default),
  ])
}

fn cursor(model: Model, conv_h: Int, cols: Int) -> List(Command) {
  case model.focused {
    True -> {
      let avail = cols - 5
      let len = int.min(string.length(model.input), avail)
      [command.MoveTo(4 + len, conv_h + 1), command.ShowCursor]
    }
    False -> [command.HideCursor]
  }
}

fn status_line(model: Model, row: Int, cols: Int) -> List(Command) {
  let text = case model.scroll > 0 {
    True ->
      "SCROLL ↑"
      <> int.to_string(model.scroll)
      <> "  ·  wheel/↑↓/PgUp·PgDn scroll  ·  o expand all  ·  ↓ latest  ·  i type"
    False ->
      case model.focused {
        True ->
          model.status
          <> "  ·  Esc: scroll/mouse mode  ·  Enter: send  ·  Ctrl+X: quit"
        False ->
          model.status
          <> "  ·  wheel/↑↓ scroll · click to expand · o all · i type · Ctrl+X quit"
      }
  }
  let #(color, attrs) = case string.starts_with(model.status, "error") {
    True -> #(style.Red, [])
    False ->
      case model.scroll > 0 {
        True -> #(style.Cyan, [])
        False -> #(style.Grey, [style.Dim])
      }
  }
  draw(0, row, truncate(text, cols), color, attrs)
}

fn network_panel(model: Model, x: Int, w: Int) -> List(Command) {
  let ws_color = case model.note {
    Some(_) -> style.Red
    None -> style.Cyan
  }
  list.flatten([
    draw(x, 1, "workspace", style.Grey, [style.Dim]),
    put(x, 2, truncate(model.project, w), ws_color),
    draw(x, 4, "policy", style.Grey, [style.Dim]),
    put(x, 5, "· bash   sandbox · net BLOCKED", style.Green),
    put(x, 6, "· files  in-process (unsandboxed)", style.Yellow),
    draw(x, 8, "live egress feed", style.Grey, [style.Dim]),
    draw(x, 9, "(pending server SSE)", style.Grey, [style.Dim]),
  ])
}

// --- Box + text drawing primitives ----------------------------------------

fn draw(
  x: Int,
  y: Int,
  text: String,
  color: Color,
  attrs: List(Attribute),
) -> List(Command) {
  [
    command.MoveTo(x, y),
    command.SetStyle(style.Style(bg: style.Default, fg: color, attributes: attrs)),
    command.Print(text),
    command.ResetStyle,
  ]
}

fn put(x: Int, y: Int, text: String, color: Color) -> List(Command) {
  draw(x, y, text, color, [])
}

/// Quiet chrome: dim grey borders with a dim title.
fn box(x: Int, y: Int, w: Int, h: Int, title: String, _color: Color) -> List(Command) {
  let prefix = "╭─ " <> title <> " "
  let fill = int.max(w - string.length(prefix) - 1, 0)
  let top = prefix <> string.repeat("─", fill) <> "╮"
  let bottom = "╰" <> string.repeat("─", int.max(w - 2, 0)) <> "╯"
  let c = style.Grey
  let a = [style.Dim]
  let sides =
    list.repeat(Nil, int.max(h - 2, 0))
    |> list.index_map(fn(_, i) {
      let r = y + i + 1
      list.flatten([draw(x, r, "│", c, a), draw(x + w - 1, r, "│", c, a)])
    })
    |> list.flatten
  list.flatten([draw(x, y, top, c, a), sides, draw(x, y + h - 1, bottom, c, a)])
}

// --- `@` autocomplete -----------------------------------------------------

fn list_project_files(project: String) -> List(String) {
  case simplifile.get_files(project) {
    Error(_) -> []
    Ok(paths) ->
      paths
      |> list.map(relativize(_, project))
      |> list.filter(is_useful_path)
      |> list.take(max_files)
  }
}

fn relativize(path: String, project: String) -> String {
  case string.starts_with(path, project <> "/") {
    True -> string.drop_start(path, string.length(project) + 1)
    False -> path
  }
}

fn is_useful_path(path: String) -> Bool {
  let noise = ["/.git/", "/build/", "/node_modules/", "/.elixir_ls/", "/_build/"]
  !string.starts_with(path, ".") && !list.any(noise, string.contains(path, _))
}

fn active_token(input: String) -> Option(String) {
  case string.split(input, "@") {
    [] | [_] -> None
    parts -> {
      let token = list.last(parts) |> result.unwrap("")
      case string.contains(token, " ") || string.contains(token, "\n") {
        True -> None
        False -> Some(token)
      }
    }
  }
}

fn suggestions_for(input: String, files: List(String)) -> List(String) {
  case active_token(input) {
    None -> []
    Some(token) -> {
      let needle = string.lowercase(token)
      files
      |> list.filter(fn(f) { string.contains(string.lowercase(f), needle) })
      |> list.take(max_suggestions)
    }
  }
}

fn accept_completion(input: String, choice: String) -> String {
  case active_token(input) {
    None -> input
    Some(token) -> string.drop_end(input, string.length(token) + 1) <> choice <> " "
  }
}

// --- Text helpers ---------------------------------------------------------

fn wrap_text(text: String, width: Int) -> List(String) {
  text
  |> string.split("\n")
  |> list.flat_map(fn(l) { wrap_line(l, int.max(width, 8)) })
}

fn wrap_line(l: String, width: Int) -> List(String) {
  case string.length(l) <= width {
    True -> [l]
    False -> {
      let #(acc, cur) =
        string.split(l, " ")
        |> list.fold(#([], ""), fn(st, word) {
          let #(acc, cur) = st
          case cur == "" {
            True -> #(acc, word)
            False ->
              case string.length(cur) + 1 + string.length(word) <= width {
                True -> #(acc, cur <> " " <> word)
                False -> #([cur, ..acc], word)
              }
          }
        })
      list.reverse(case cur {
        "" -> acc
        _ -> [cur, ..acc]
      })
    }
  }
}

fn wrap_plain(text: String, width: Int) -> List(CLine) {
  wrap_text(text, width) |> list.map(fn(l) { line(l, style.Default) })
}

fn wrap_styled(text: String, width: Int, color: Color) -> List(CLine) {
  wrap_text(text, width) |> list.map(fn(l) { line(l, color) })
}

fn format_tool_call(name: String, input: String) -> String {
  let summary = case name {
    "bash" -> json_field(input, "command")
    "read" | "write" | "edit" -> json_field(input, "path")
    _ -> Error(Nil)
  }
  case summary {
    Ok(value) -> name <> "  " <> truncate(value, 120)
    Error(_) -> name <> "  " <> truncate(input, 120)
  }
}

fn json_field(input: String, key: String) -> Result(String, Nil) {
  json.parse(input, {
    use value <- decode.field(key, decode.string)
    decode.success(value)
  })
  |> result.replace_error(Nil)
}

fn strip_prefix(s: String, prefix: String) -> Result(String, Nil) {
  case string.starts_with(s, prefix) {
    True -> Ok(string.drop_start(s, string.length(prefix)))
    False -> Error(Nil)
  }
}

fn truncate(s: String, n: Int) -> String {
  case string.length(s) > n {
    True -> string.slice(s, 0, int.max(n - 1, 0)) <> "…"
    False -> s
  }
}

fn unsandboxable_note(project: String) -> Option(String) {
  let home = envoy.get("HOME") |> result.unwrap("")
  case home != "" && string.starts_with(home <> "/", project <> "/") {
    True ->
      Some(
        "⚠ nono can't sandbox this directory (it contains nono's own state). "
        <> "Quit with Ctrl+X and run `bough` from a project subdirectory.",
      )
    False -> None
  }
}
