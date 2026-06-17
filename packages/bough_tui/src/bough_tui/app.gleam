//// The bough TUI application (shore / The Elm Architecture).
////
//// Layout: a conversation pane beside a network side pane, an input line, and
//// a status line (SPEC.md §9). Network calls run as shore effects (separate
//// processes) so the UI stays responsive while the agent works.
////
//// Not yet wired: live egress streaming into the network pane and rule editing
//// (need server SSE + a rules endpoint), and the session-tree overlay.

import bough_tui/client
import envoy
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import shore
import shore/layout
import shore/style
import shore/ui

const default_server = "http://127.0.0.1:4096"

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
  )
}

pub type Msg {
  SessionCreated(Result(String, String))
  InputChanged(String)
  Submit
  AgentReplied(Result(String, String))
}

pub fn init() -> #(Model, List(fn() -> Msg)) {
  let server = envoy.get("BOUGH_SERVER") |> result.unwrap(default_server)
  let project = envoy.get("PWD") |> result.unwrap(".")
  let model =
    Model(
      server: server,
      project: project,
      session: None,
      input: "",
      chat: [],
      status: "connecting to " <> server <> " …",
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
        chat: [ChatLine("bough", text), ..model.chat],
      ),
      [],
    )
    AgentReplied(Error(e)) -> #(
      Model(
        ..model,
        status: "error",
        chat: [ChatLine("error", e), ..model.chat],
      ),
      [],
    )
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
          chat: [ChatLine("you", text), ..model.chat],
        )
      let server = model.server
      #(model, [fn() { AgentReplied(client.send_message(server, id, text)) }])
    }
    _, _ -> #(model, [])
  }
}

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
  let lines = case model.chat {
    [] -> [ui.text("Press Tab to focus the input, type a task, Enter to send.")]
    chat ->
      chat
      |> list.reverse
      |> list.map(fn(line) { ui.text_wrapped(line.role <> ": " <> line.text) })
  }
  ui.box(lines, Some("conversation"))
}

fn message_input(model: Model) -> shore.Node(Msg) {
  ui.box(
    [ui.input_submit("> ", model.input, style.Fill, InputChanged, Submit, False)],
    Some("message — Tab to focus"),
  )
}

fn status(model: Model) -> shore.Node(Msg) {
  ui.text(model.status <> "   ·   Tab: focus   Enter: send   Ctrl+X: quit")
}

fn network(_model: Model) -> shore.Node(Msg) {
  ui.box(
    [
      ui.text("policy"),
      ui.text("· bash  → sandbox, network BLOCKED"),
      ui.text("· files → in-process (unsandboxed)"),
      ui.br(),
      ui.text("live egress feed:"),
      ui.text("(pending server SSE)"),
    ],
    Some("network"),
  )
}
