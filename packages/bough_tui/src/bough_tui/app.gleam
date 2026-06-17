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
import shore/layout
import shore/style
import shore/ui

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
  )
}

pub type Msg {
  SessionCreated(Result(String, String))
  InputChanged(String)
  Submit
  Started(Result(Nil, String))
  Polled(Result(client.RunState, String))
  Tick
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
    )
  #(model, [fn() { SessionCreated(client.create_session(server, project)) }])
}

pub fn update(model: Model, msg: Msg) -> #(Model, List(fn() -> Msg)) {
  case msg {
    SessionCreated(Ok(id)) -> #(
      Model(..model, session: Some(id), status: "ready · session " <> id),
      [],
    )
    SessionCreated(Error(e)) -> #(Model(..model, status: "error: " <> e), [])

    InputChanged(value) -> #(Model(..model, input: value), [])

    Submit -> submit(model)

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

// --- View ----------------------------------------------------------------

pub fn view(model: Model) -> shore.Node(Msg) {
  layout.grid(
    gap: 1,
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

fn conversation(model: Model) -> shore.Node(Msg) {
  let history = model.chat |> list.reverse |> list.flat_map(render_entry)
  let live = case model.pending {
    True ->
      list.append(render_entry(Bough(model.running_steps)), [
        thinking_block(model.frame),
      ])
    False -> []
  }
  let banner = case model.note {
    Some(text) -> [ui.text_wrapped_styled(text, Some(style.Red), None), ui.text("")]
    None -> []
  }
  let main = case history, live {
    [], [] -> [
      ui.text_styled(
        "Tab to focus · type a task · Enter to send",
        Some(style.Cyan),
        None,
      ),
    ]
    _, _ -> list.append(history, live)
  }
  ui.box_styled(list.append(banner, main), Some("conversation"), Some(style.Blue))
}

fn render_entry(entry: Entry) -> List(shore.Node(Msg)) {
  case entry {
    You(text) -> [header("▌ you", style.Cyan), ui.text_wrapped(text), ui.text("")]
    Failed(e) -> [header("▌ error", style.Red), ui.text_wrapped(e), ui.text("")]
    Bough(steps) ->
      list.flatten([
        [header("▌ bough", style.Green)],
        list.flat_map(steps, render_step),
        [ui.text("")],
      ])
  }
}

fn render_step(step: Step) -> List(shore.Node(Msg)) {
  case step {
    Text(text) -> render_markdown(text)
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
/// else (tables, code) is left as wrapped text.
fn render_markdown(text: String) -> List(shore.Node(Msg)) {
  text
  |> string.split("\n")
  |> list.map(render_md_line)
}

fn render_md_line(line: String) -> shore.Node(Msg) {
  let trimmed = string.trim_end(line)
  case
    strip_prefix(trimmed, "### "),
    strip_prefix(trimmed, "## "),
    strip_prefix(trimmed, "# ")
  {
    Ok(h), _, _ | _, Ok(h), _ | _, _, Ok(h) ->
      ui.text_styled(h, Some(style.Magenta), None)
    _, _, _ ->
      case trimmed == "---" || trimmed == "***" || trimmed == "___" {
        True -> ui.hr()
        False -> ui.text_wrapped(line)
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
  let color = case string.starts_with(model.status, "error") {
    True -> style.Red
    False -> style.Magenta
  }
  ui.text_styled(
    model.status <> "   ·   Tab: focus   Enter: send   Ctrl+X: quit",
    Some(color),
    None,
  )
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
