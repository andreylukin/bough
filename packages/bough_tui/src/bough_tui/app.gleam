//// The bough TUI application (shore / The Elm Architecture).
////
//// Layout (SPEC.md §9): a conversation pane beside a network side pane, an
//// input line, and a status line. Assistant turns render their transcript —
//// intermediate text, tool calls, and (truncated) tool results — as colored
//// blocks. An animated spinner plays while the agent works. Live streaming of
//// these steps is a later SSE refinement.

import bough_tui/client.{type Step, Text, ToolCall, ToolResult}
import envoy
import gleam/dynamic/decode
import gleam/erlang/process
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import shore
import shore/key
import shore/layout
import shore/style
import shore/ui
import simplifile

const default_server = "http://127.0.0.1:4096"

const spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]

const result_lines = 6

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
    // Live transcript of the in-flight run, replaced on each poll.
    running_steps: List(Step),
    // Set when the workspace can't be sandboxed by nono (home or an ancestor).
    note: Option(String),
    // Workspace files (relative paths) for `@` autocomplete, scanned once.
    files: List(String),
    // Current `@`-mention matches for the active token; empty when inactive.
    suggestions: List(String),
    // Rows scrolled up from the bottom; 0 follows the latest output.
    scroll: Int,
  )
}

pub type Msg {
  SessionCreated(Result(String, String))
  FilesScanned(List(String))
  InputChanged(String)
  Submit
  Started(Result(Nil, String))
  Polled(Result(client.RunState, String))
  Tick
  // Scroll the transcript by N rows (positive = back into history).
  ScrollBy(Int)
}

pub fn init() -> #(Model, List(fn() -> Msg)) {
  let server = envoy.get("BOUGH_SERVER") |> result.unwrap(default_server)
  // BOUGH_PROJECT is set by the `bough` shell function to the directory you
  // launched from (the package's own cwd would otherwise leak in via PWD).
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
    )
  // Both effects run off the init path (the actor initialiser has a ~1s
  // budget); scanning the workspace synchronously here would time it out.
  #(model, [
    fn() { SessionCreated(client.create_session(server, project)) },
    fn() { FilesScanned(list_project_files(project)) },
  ])
}

pub fn update(model: Model, msg: Msg) -> #(Model, List(fn() -> Msg)) {
  case msg {
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
        // Typing snaps the view back to the latest output.
        scroll: 0,
      ),
      [],
    )

    ScrollBy(n) -> {
      // Clamp to the real range so the counter can't overshoot the top (which
      // would make the first presses back down do nothing). The +2 matches the
      // two marker rows scroll_window reserves once scrolled, so the very first
      // line is reachable.
      let max_scroll =
        int.max(list.length(transcript(model)) - conversation_rows() + 2, 0)
      #(Model(..model, scroll: int.clamp(model.scroll + n, 0, max_scroll)), [])
    }

    Submit ->
      case model.suggestions {
        // A completion is active: Enter accepts the top match instead of
        // sending. A second Enter (now with no active token) sends.
        [top, ..] -> #(
          Model(
            ..model,
            input: accept_completion(model.input, top),
            suggestions: [],
          ),
          [],
        )
        [] -> submit(model)
      }

    Started(Ok(_)) ->
      case model.session {
        Some(id) -> #(model, [poll(model.server, id)])
        None -> #(model, [])
      }
    Started(Error(e)) -> #(
      Model(..model, status: "error", pending: False, chat: [Failed(e), ..model.chat]),
      [],
    )

    Polled(Ok(run)) -> polled(model, run)
    Polled(Error(_)) ->
      // Transient (e.g. server briefly unavailable); keep polling while pending.
      case model.session, model.pending {
        Some(id), True -> #(model, [poll(model.server, id)])
        _, _ -> #(model, [])
      }

    Tick ->
      case model.pending {
        True -> #(Model(..model, frame: model.frame + 1), [tick])
        False -> #(model, [])
      }
  }
}

fn submit(model: Model) -> #(Model, List(fn() -> Msg)) {
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

/// Self-scheduling poll (~400ms) for run progress.
fn poll(server: String, id: String) -> fn() -> Msg {
  fn() {
    process.sleep(400)
    Polled(client.get_run(server, id))
  }
}

/// Self-scheduling animation tick (~120ms) for the thinking spinner.
fn tick() -> Msg {
  process.sleep(120)
  Tick
}

/// nono keeps its protected state inside $HOME, so it refuses to sandbox the
/// home directory or any ancestor of it. Warn instead of failing mid-task.
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

// --- `@` autocomplete ----------------------------------------------------

const max_suggestions = 8

const max_files = 2000

/// Scan the workspace once for files to offer as `@`-mentions. Heavy/noise
/// directories are skipped; paths are made relative to the project root.
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
  let hidden = string.starts_with(path, ".")
  !hidden && !list.any(noise, string.contains(path, _))
}

/// The text after the last unbroken `@…` in the input — the active mention
/// token — or `None` when there is no live mention to complete.
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

/// Replace the active `@token` with the chosen path (and a trailing space).
fn accept_completion(input: String, choice: String) -> String {
  case active_token(input) {
    None -> input
    Some(token) ->
      string.drop_end(input, string.length(token) + 1) <> choice <> " "
  }
}

// --- View ----------------------------------------------------------------

pub fn view(model: Model) -> shore.Node(Msg) {
  // gap MUST stay 0: shore's calc_sizes allocates the full height to the rows
  // and then adds gaps on top, so any gap overflows the grid and pushes the
  // status line off the bottom of the screen.
  layout.grid(
    gap: 0,
    rows: [style.Fill, style.Px(3), style.Px(1)],
    cols: [style.Fill, style.Pct(32)],
    cells: [
      layout.cell(conversation(model), #(0, 0), #(0, 0)),
      layout.cell(network(model), #(0, 0), #(1, 1)),
      layout.cell(message_input(model), #(1, 1), #(0, 1)),
      layout.cell(status(model), #(2, 2), #(0, 1)),
    ],
  )
}

/// All transcript rows (banner, history, in-flight steps, `@` picker) as one
/// node-per-visual-row list. Shared by the view and by scroll clamping so the
/// row budget and scroll bounds always agree.
fn transcript(model: Model) -> List(shore.Node(Msg)) {
  let width = conv_width()
  let history =
    model.chat |> list.reverse |> list.flat_map(render_entry(_, width))
  let live = case model.pending {
    True ->
      list.append(render_entry(Bough(model.running_steps), width), [
        thinking_block(model.frame),
      ])
    False -> []
  }
  let banner = case model.note {
    Some(text) -> list.append(wrap_styled(text, width, style.Red), [ui.text("")])
    None -> []
  }
  let main = case history, live {
    [], [] -> [
      ui.text_styled(
        "Tab to focus · type a task · Enter to send · @ to mention a file",
        Some(style.Cyan),
        None,
      ),
    ]
    _, _ -> list.append(history, live)
  }
  list.flatten([banner, main, suggestion_view(model.suggestions)])
}

fn conversation(model: Model) -> shore.Node(Msg) {
  // shore renders every child top-down with no clipping, so an overflowing
  // conversation would spill past the box into the input. Show a window of the
  // rows that fit, offset by the scroll position (0 = follow the latest).
  let content =
    scroll_window(transcript(model), conversation_rows(), model.scroll)
  // KeyBinds fire only while the input is unfocused (press Esc), so scrolling
  // is a deliberate mode — shore swallows these keys while you're typing.
  ui.box_styled(
    list.append(content, scroll_keys()),
    Some("conversation"),
    Some(style.Blue),
  )
}

/// Non-visible keybinds that scroll the transcript. They only trigger when no
/// input is focused (Esc clears focus), since shore routes keys to a focused
/// input first.
fn scroll_keys() -> List(shore.Node(Msg)) {
  [
    ui.keybind(key.Up, ScrollBy(1)),
    ui.keybind(key.Down, ScrollBy(-1)),
    ui.keybind(key.PageUp, ScrollBy(10)),
    ui.keybind(key.PageDown, ScrollBy(-10)),
    ui.keybind(key.Char("k"), ScrollBy(1)),
    ui.keybind(key.Char("j"), ScrollBy(-1)),
  ]
}

/// The visible window of transcript rows. With more rows than fit, a marker
/// tops the pane (and bottoms it when scrolled up) so position is legible.
fn scroll_window(
  nodes: List(shore.Node(Msg)),
  budget: Int,
  scroll: Int,
) -> List(shore.Node(Msg)) {
  let total = list.length(nodes)
  case total <= budget {
    True -> nodes
    False -> {
      let scrolled = scroll > 0
      // Reserve a row for the top marker (always, since total > budget) and the
      // bottom marker (only when scrolled up off the latest output).
      let inner = int.max(budget - 1 - bool_int(scrolled), 1)
      let max_scroll = total - inner
      let s = int.clamp(scroll, 0, max_scroll)
      let start = int.max(total - inner - s, 0)
      let visible = nodes |> list.drop(start) |> list.take(inner)
      let above = start
      let below = total - start - inner
      let top = case above > 0 {
        True -> [marker("⋯ " <> int.to_string(above) <> " above")]
        False -> []
      }
      let bottom = case below > 0 {
        True -> [marker("⋯ " <> int.to_string(below) <> " below")]
        False -> []
      }
      list.flatten([top, visible, bottom])
    }
  }
}

fn marker(text: String) -> shore.Node(Msg) {
  ui.text_styled(text, Some(style.Blue), None)
}

fn bool_int(b: Bool) -> Int {
  case b {
    True -> 1
    False -> 0
  }
}

/// Rows available inside the conversation box: terminal height minus the input
/// box (3), status line (1) and the box border (2). gap is 0 (see view).
fn conversation_rows() -> Int {
  case term_rows() {
    Ok(n) -> int.max(n - 6, 5)
    Error(_) -> 20
  }
}

/// Text columns inside the conversation box: the Fill grid cell (terminal
/// width minus the 32%% network pane and the gap) minus the box chrome. Kept
/// slightly narrow so our wrapping never under-counts shore's own wrapping
/// (which would let content overflow the box again).
fn conv_width() -> Int {
  case term_cols() {
    Ok(n) -> int.max(n * 68 / 100 - 6, 20)
    Error(_) -> 80
  }
}

type IoError

@external(erlang, "io", "rows")
fn term_rows() -> Result(Int, IoError)

@external(erlang, "io", "columns")
fn term_cols() -> Result(Int, IoError)

/// Wrap text to `width` columns, one returned string per visual row, so that
/// counting nodes equals counting rows (what `tail_to_fit` budgets on).
fn wrap_text(text: String, width: Int) -> List(String) {
  text
  |> string.split("\n")
  |> list.flat_map(fn(line) { wrap_line(line, int.max(width, 8)) })
}

fn wrap_line(line: String, width: Int) -> List(String) {
  case string.length(line) <= width {
    True -> [line]
    False -> {
      let #(acc, cur) =
        string.split(line, " ")
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

fn wrap_plain(text: String, width: Int) -> List(shore.Node(Msg)) {
  wrap_text(text, width) |> list.map(ui.text)
}

fn wrap_styled(
  text: String,
  width: Int,
  color: style.Color,
) -> List(shore.Node(Msg)) {
  wrap_text(text, width)
  |> list.map(fn(l) { ui.text_styled(l, Some(color), None) })
}

/// A compact `@`-mention picker; the top match (Enter to accept) is arrowed.
fn suggestion_view(suggestions: List(String)) -> List(shore.Node(Msg)) {
  case suggestions {
    [] -> []
    _ -> {
      let header =
        ui.text_styled("@ files · Enter completes top", Some(style.Magenta), None)
      let rows =
        list.index_map(suggestions, fn(path, i) {
          case i {
            0 -> ui.text_styled("→ " <> path, Some(style.Cyan), None)
            _ -> ui.text_styled("  " <> path, Some(style.White), None)
          }
        })
      [ui.hr(), header, ..rows]
    }
  }
}

fn render_entry(entry: Entry, width: Int) -> List(shore.Node(Msg)) {
  case entry {
    You(text) ->
      list.flatten([
        [header("▌ you", style.Cyan)],
        wrap_plain(text, width),
        [ui.text("")],
      ])
    Failed(e) ->
      list.flatten([
        [header("▌ error", style.Red)],
        wrap_plain(e, width),
        [ui.text("")],
      ])
    Bough(steps) ->
      list.flatten([
        [header("▌ bough", style.Green)],
        list.flat_map(steps, render_step(_, width)),
        [ui.text("")],
      ])
  }
}

fn render_step(step: Step, width: Int) -> List(shore.Node(Msg)) {
  case step {
    Text(text) -> render_markdown(text, width)
    ToolCall(name, input) -> [
      ui.text_styled("⚙ " <> format_tool_call(name, input), Some(style.Yellow), None),
    ]
    ToolResult(_name, output) -> render_result(output)
  }
}

/// Human-readable tool call: `bash ls`, `read path`, etc., falling back to the
/// raw JSON input when the expected field is missing.
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

fn render_result(output: String) -> List(shore.Node(Msg)) {
  case string.trim_end(output) {
    "" -> [rail("(no output)"), ui.text("")]
    trimmed -> {
      let lines = string.split(trimmed, "\n")
      let shown =
        lines |> list.take(result_lines) |> list.map(fn(l) { rail(truncate(l, 160)) })
      let more = case list.length(lines) - result_lines {
        extra if extra > 0 -> [
          rail("…(+" <> int.to_string(extra) <> " more lines)"),
        ]
        _ -> []
      }
      list.flatten([shown, more, [ui.text("")]])
    }
  }
}

/// A tool-output line, indented behind a dim left rail.
fn rail(text: String) -> shore.Node(Msg) {
  ui.text_styled("  │ " <> text, Some(style.Blue), None)
}

fn header(label: String, color: style.Color) -> shore.Node(Msg) {
  ui.text_styled(label, Some(color), None)
}

/// Lightweight markdown: style headings and turn rules into lines. Everything
/// else (tables, code) is wrapped to one node per visual row.
fn render_markdown(text: String, width: Int) -> List(shore.Node(Msg)) {
  text
  |> string.split("\n")
  |> list.flat_map(render_md_line(_, width))
}

fn render_md_line(line: String, width: Int) -> List(shore.Node(Msg)) {
  let trimmed = string.trim_end(line)
  case
    strip_prefix(trimmed, "### "),
    strip_prefix(trimmed, "## "),
    strip_prefix(trimmed, "# ")
  {
    Ok(h), _, _ | _, Ok(h), _ | _, _, Ok(h) -> [
      ui.text_styled(h, Some(style.Magenta), None),
    ]
    _, _, _ ->
      case trimmed == "---" || trimmed == "***" || trimmed == "___" {
        True -> [ui.hr()]
        False -> wrap_plain(line, width)
      }
  }
}

fn strip_prefix(s: String, prefix: String) -> Result(String, Nil) {
  case string.starts_with(s, prefix) {
    True -> Ok(string.drop_start(s, string.length(prefix)))
    False -> Error(Nil)
  }
}

fn truncate(s: String, n: Int) -> String {
  case string.length(s) > n {
    True -> string.slice(s, 0, n) <> "…"
    False -> s
  }
}

fn thinking_block(frame: Int) -> shore.Node(Msg) {
  let glyph = case list.drop(spinner, frame % 10) {
    [g, ..] -> g
    [] -> "⠋"
  }
  ui.text_styled(glyph <> " thinking …", Some(style.Yellow), None)
}

fn message_input(model: Model) -> shore.Node(Msg) {
  ui.box_styled(
    [ui.input_submit("> ", model.input, style.Fill, InputChanged, Submit, False)],
    Some("message — Tab to focus"),
    Some(style.Cyan),
  )
}

fn status(model: Model) -> shore.Node(Msg) {
  case model.scroll > 0 {
    True ->
      ui.text_styled(
        "SCROLL ↑"
          <> int.to_string(model.scroll)
          <> "   ·   ↑/↓ PgUp/PgDn (or j/k) scroll   ·   ↓ to latest   ·   Tab: type",
        Some(style.Yellow),
        None,
      )
    False -> {
      let color = case string.starts_with(model.status, "error") {
        True -> style.Red
        False -> style.Magenta
      }
      ui.text_styled(
        model.status
          <> "   ·   Esc then ↑/↓: scroll   ·   Enter: send   Ctrl+X: quit",
        Some(color),
        None,
      )
    }
  }
}

fn workspace_color(model: Model) -> style.Color {
  case model.note {
    Some(_) -> style.Red
    None -> style.Cyan
  }
}

fn network(model: Model) -> shore.Node(Msg) {
  ui.box_styled(
    [
      ui.text_styled("workspace", Some(style.White), None),
      ui.text_wrapped_styled(model.project, Some(workspace_color(model)), None),
      ui.br(),
      ui.text_styled("policy", Some(style.White), None),
      ui.text_styled("· bash   sandbox · net BLOCKED", Some(style.Green), None),
      ui.text_styled("· files  in-process (unsandboxed)", Some(style.Yellow), None),
      ui.br(),
      ui.text_styled("live egress feed", Some(style.White), None),
      ui.text_styled("(pending server SSE)", Some(style.Blue), None),
    ],
    Some("network"),
    Some(style.Magenta),
  )
}
