//// The bough TUI application (shore / The Elm Architecture).
////
//// Layout (SPEC.md §9): a conversation pane beside a network side pane, an
//// input line, and a status line. Network calls run as shore effects so the
//// UI stays responsive; an animated "thinking" block plays while the agent
//// works. Live egress streaming and the tree overlay are still pending.

import bough_tui/client
import envoy
import gleam/erlang/process
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

pub type ChatLine {
  ChatLine(role: String, text: String)
}

pub type Model {
  Model(
    server: String,
    project: String,
    session: Option(String),
    input: String,
    // Newest first; reversed for display.
    chat: List(ChatLine),
    status: String,
    pending: Bool,
    frame: Int,
  )
}

pub type Msg {
  SessionCreated(Result(String, String))
  InputChanged(String)
  Submit
  AgentReplied(Result(String, String))
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

    AgentReplied(Ok(text)) -> #(
      Model(
        ..model,
        status: "ready",
        pending: False,
        chat: [ChatLine("bough", text), ..model.chat],
      ),
      [],
    )
    AgentReplied(Error(e)) -> #(
      Model(
        ..model,
        status: "error",
        pending: False,
        chat: [ChatLine("error", e), ..model.chat],
      ),
      [],
    )

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
          chat: [ChatLine("you", text), ..model.chat],
        )
      let server = model.server
      #(model, [
        fn() { AgentReplied(client.send_message(server, id, text)) },
        tick,
      ])
    }
    _, _ -> #(model, [])
  }
}

/// Self-scheduling animation tick (~120ms) for the thinking spinner.
fn tick() -> Msg {
  process.sleep(120)
  Tick
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
  let history = model.chat |> list.reverse |> list.flat_map(render_message)
  let thinking = case model.pending {
    True -> [thinking_block(model.frame)]
    False -> []
  }
  let body = case history, thinking {
    [], [] -> [
      ui.text_styled(
        "Tab to focus · type a task · Enter to send",
        Some(style.Cyan),
        None,
      ),
    ]
    _, _ -> list.append(history, thinking)
  }
  ui.box_styled(body, Some("conversation"), Some(style.Blue))
}

fn render_message(line: ChatLine) -> List(shore.Node(Msg)) {
  let #(label, color) = case line.role {
    "you" -> #("▌ you", style.Cyan)
    "bough" -> #("▌ bough", style.Green)
    _ -> #("▌ error", style.Red)
  }
  let header = ui.text_styled(label, Some(color), None)
  let body = case line.role {
    "bough" -> render_markdown(line.text)
    _ -> [ui.text_wrapped(line.text)]
  }
  list.flatten([[header], body, [ui.text("")]])
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

fn network(_model: Model) -> shore.Node(Msg) {
  ui.box_styled(
    [
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
