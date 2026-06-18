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

import bough_tui/client.{
  type Step, Call, Check, Exec, Plan, Review, Text, ToolCall, ToolResult, Worker,
}
import etch/command.{type Command}
import etch/event.{type Event}
import etch/stdout
import etch/style.{type Attribute, type Color}
import etch/terminal
import envoy
import gleam/bit_array
import gleam/dict.{type Dict}
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

// Nerd Font glyphs (need a patched font in the terminal). Swap the code points
// if your font maps them elsewhere — e.g. nf-md-hand_wave for a literal wave.
const glyph_check = "\u{f00c}"

// nf-fa-check
const glyph_wave = "\u{f256}"

// nf-fa-hand_paper_o (raised "hi" hand)

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
    // Caret position as a character index into `input` (0..length).
    cursor: Int,
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
    // Which screen is showing: the chat, or a picker overlay.
    view: View,
    // Sessions for the resume picker.
    sessions: List(client.Summary),
    // The loaded session tree (for the tree overlay), and its flattened rows.
    tree: Option(client.Tree),
    tree_rows: List(TreeRow),
    // Selected row in the active overlay.
    sel: Int,
    // Set by --continue: resume this project's latest session once listed.
    auto_continue: Bool,
    // Active mouse text-selection in the conversation pane, in screen coords.
    mouse_sel: Option(Region),
    // While dragging past an edge: signed lines to scroll per timer tick
    // (>0 older/up, <0 newer/down, 0 = not autoscrolling).
    autoscroll: Int,
    // Supervisor context tokens from the latest run poll, for the status meter.
    context_tokens: Int,
    // Supervisor model in use (from the server's /config), shown in the status line.
    model_name: String,
  )
}

/// A mouse text-selection over the conversation pane. Rows are absolute
/// transcript line indices (not screen rows) so the selection survives
/// scrolling mid-drag; columns are screen columns (which don't scroll).
/// `anchor` is where the drag started, `head` follows the cursor; either may
/// come before the other.
pub type Region {
  Region(anchor_line: Int, anchor_col: Int, head_line: Int, head_col: Int)
}

pub type View {
  ChatV
  SessionsV
  TreeV
}

/// One node of the flattened session tree shown in the tree overlay. `prefix`
/// is the connector gutter (`├─ `, `│  `, `└─ `); linear runs keep a straight
/// (empty) gutter so only real forks indent. `fork_id` is the entry to branch
/// from on Enter (empty for in-progress live steps, which aren't forkable yet).
pub type TreeRow {
  TreeRow(
    prefix: String,
    label: String,
    color: Color,
    active: Bool,
    fork_id: String,
  )
}

pub type Msg {
  EtchEvent(Event)
  SessionCreated(Result(String, String))
  FilesScanned(List(String))
  ConfigLoaded(Result(#(String, String), String))
  Started(Result(Nil, String))
  Polled(Result(client.RunState, String))
  Tick
  // Internal messages produced by translating events.
  InputChanged(String, Int)
  Submit
  ScrollBy(Int)
  ToggleResult(Int)
  ToggleAll
  SetFocus(Bool)
  Quit
  // Resume / branch overlays.
  OpenSessions
  SessionsLoaded(Result(List(client.Summary), String))
  OpenTree
  TreeLoaded(Result(client.Tree, String))
  PickMove(Int)
  PickChoose
  CloseOverlay
  Resumed(Result(client.Tree, String))
  Forked(Result(client.Tree, String))
  // Returned by the clipboard-copy side effect; no model change.
  Noop
  // Timer tick that advances an in-progress edge autoscroll.
  AutoScroll
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
      cursor: 0,
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
      view: ChatV,
      sessions: [],
      tree: None,
      tree_rows: [],
      sel: 0,
      auto_continue: envoy.get("BOUGH_CONTINUE") |> result.is_ok,
      mouse_sel: None,
      autoscroll: 0,
      context_tokens: 0,
      model_name: "",
    )
  let resume = envoy.get("BOUGH_RESUME") |> result.is_ok
  // --resume opens the picker on launch; --continue resumes silently once the
  // session list arrives (handled in SessionsLoaded).
  let model = case resume {
    True -> Model(..model, view: SessionsV, status: "resume a session …")
    False -> model
  }
  let base = [
    fn() { SessionCreated(client.create_session(server, project)) },
    fn() { FilesScanned(list_project_files(project)) },
    fn() { ConfigLoaded(client.get_config(server)) },
  ]
  let effects = case resume || model.auto_continue {
    True -> [fn() { SessionsLoaded(client.list_sessions(server)) }, ..base]
    False -> base
  }
  #(model, effects)
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

    Noop -> #(model, [])

    AutoScroll ->
      case model.mouse_sel, model.autoscroll {
        Some(r), step if step != 0 -> {
          let #(_cols, _rows, conv_w, _conv_h) = dims(model)
          let before = model.scroll
          let model = scroll_by(model, step)
          case model.scroll == before {
            // Reached the top/bottom: stop the loop.
            True -> #(Model(..model, autoscroll: 0), [])
            False -> {
              let #(head_line, head_col) = case step > 0 {
                True -> #(top_visible_line(model), 2)
                False -> #(bottom_visible_line(model), conv_w - 3)
              }
              #(
                Model(
                  ..model,
                  mouse_sel: Some(Region(r.anchor_line, r.anchor_col, head_line, head_col)),
                ),
                [autoscroll_tick],
              )
            }
          }
        }
        // Selection ended or autoscroll cleared → let the loop die.
        _, _ -> #(model, [])
      }

    SessionCreated(Ok(id)) -> #(
      Model(..model, session: Some(id), status: "ready · session " <> id),
      [],
    )
    SessionCreated(Error(e)) -> #(Model(..model, status: "error: " <> e), [])

    FilesScanned(files) -> #(Model(..model, files: files), [])

    ConfigLoaded(Ok(#(_provider, name))) -> #(
      Model(..model, model_name: name),
      [],
    )
    ConfigLoaded(Error(_)) -> #(model, [])

    InputChanged(value, caret) -> #(
      Model(
        ..model,
        input: value,
        cursor: int.clamp(caret, 0, string.length(value)),
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

    OpenSessions -> {
      let server = model.server
      #(Model(..model, view: SessionsV, sel: 0, status: "loading sessions …"), [
        fn() { SessionsLoaded(client.list_sessions(server)) },
      ])
    }
    SessionsLoaded(Ok(sessions)) -> {
      let model = Model(..model, sessions: sessions, sel: 0)
      case model.auto_continue {
        True -> {
          let server = model.server
          // Resume this project's most recent session (sessions are newest-first).
          case list.find(sessions, fn(s) { s.project == model.project }) {
            Ok(s) -> #(Model(..model, auto_continue: False, status: "continuing …"), [
              fn() { Resumed(client.get_session(server, s.id)) },
            ])
            Error(_) -> #(
              Model(..model, auto_continue: False, status: "no previous session here — new session"),
              [],
            )
          }
        }
        False -> #(model, [])
      }
    }
    SessionsLoaded(Error(e)) -> #(
      Model(..model, view: ChatV, status: "error: " <> e),
      [],
    )

    OpenTree ->
      case model.session {
        Some(id) -> {
          let server = model.server
          #(Model(..model, view: TreeV, sel: 0, status: "loading history …"), [
            fn() { TreeLoaded(client.get_session(server, id)) },
          ])
        }
        None -> #(model, [])
      }
    TreeLoaded(Ok(tree)) -> {
      let model = rebuild_tree(Model(..model, tree: Some(tree)))
      // Open focused on the current leaf.
      let active = index_where(model.tree_rows, fn(r) { r.active })
      #(Model(..model, sel: int.max(active, 0)), [])
    }
    TreeLoaded(Error(e)) -> #(
      Model(..model, view: ChatV, status: "error: " <> e),
      [],
    )

    PickMove(n) -> #(Model(..model, sel: clamp_sel(model, model.sel + n)), [])

    PickChoose -> pick_choose(model)

    CloseOverlay -> #(Model(..model, view: ChatV), [])

    Resumed(Ok(tree)) -> #(
      Model(
        ..model,
        view: ChatV,
        session: Some(tree.id),
        project: tree.project,
        chat: chat_from_tree(tree),
        scroll: 0,
        status: "resumed · session " <> tree.id,
      ),
      [],
    )
    Resumed(Error(e)) -> #(Model(..model, view: ChatV, status: "error: " <> e), [])

    Forked(Ok(tree)) -> #(
      Model(
        ..model,
        view: ChatV,
        chat: chat_from_tree(tree),
        scroll: 0,
        status: "branched · type to continue from here",
      ),
      [],
    )
    Forked(Error(e)) -> #(Model(..model, view: ChatV, status: "error: " <> e), [])
  }
}

fn clamp_sel(model: Model, n: Int) -> Int {
  let len = case model.view {
    SessionsV -> list.length(model.sessions)
    TreeV -> list.length(model.tree_rows)
    ChatV -> 0
  }
  int.clamp(n, 0, int.max(len - 1, 0))
}

fn pick_choose(model: Model) -> #(Model, List(fn() -> Msg)) {
  let server = model.server
  case model.view {
    SessionsV ->
      case list_at(model.sessions, model.sel) {
        Ok(s) -> #(Model(..model, status: "resuming …"), [
          fn() { Resumed(client.get_session(server, s.id)) },
        ])
        Error(_) -> #(model, [])
      }
    TreeV ->
      case model.session, list_at(model.tree_rows, model.sel) {
        // Live (in-progress) rows carry no fork_id and aren't forkable.
        Some(_), Ok(TreeRow(fork_id: "", ..)) -> #(model, [])
        Some(id), Ok(row) -> #(Model(..model, status: "branching …"), [
          fn() { Forked(client.fork(server, id, row.fork_id)) },
        ])
        _, _ -> #(model, [])
      }
    ChatV -> #(model, [])
  }
}

fn list_at(items: List(a), i: Int) -> Result(a, Nil) {
  items |> list.drop(i) |> list.first
}

/// The active branch of a fetched tree (root→active_leaf, oldest first).
fn branch_path(tree: client.Tree) -> List(client.TreeEntry) {
  case tree.active_leaf {
    "" -> []
    leaf -> walk_branch(tree.entries, leaf, [])
  }
}

fn walk_branch(
  entries: List(client.TreeEntry),
  id: String,
  acc: List(client.TreeEntry),
) -> List(client.TreeEntry) {
  case list.find(entries, fn(e) { e.id == id }) {
    Ok(e) -> {
      let acc = [e, ..acc]
      case e.parent_id {
        "" -> acc
        parent -> walk_branch(entries, parent, acc)
      }
    }
    Error(_) -> acc
  }
}

// --- Tree overlay (pi-mono style) -----------------------------------------

/// Recompute `tree_rows` from the loaded tree, folding in any in-progress run
/// steps (which aren't persisted until the turn finishes) as live rows under
/// the active leaf. No-op when no tree is loaded.
fn rebuild_tree(model: Model) -> Model {
  case model.tree {
    None -> model
    Some(tree) -> {
      let rows = build_tree_rows(tree)
      let rows = case model.pending, model.running_steps {
        True, [_, ..] -> merge_live_steps(rows, model.running_steps)
        _, _ -> rows
      }
      Model(..model, tree_rows: rows)
    }
  }
}

/// Reload the tree from the server when the overlay is open (so it reflects a
/// just-persisted turn); otherwise no effect.
fn tree_reload_effect(model: Model) -> List(fn() -> Msg) {
  case model.view, model.session {
    TreeV, Some(id) -> {
      let server = model.server
      [fn() { TreeLoaded(client.get_session(server, id)) }]
    }
    _, _ -> []
  }
}

/// Append the in-progress steps as a straight run beneath the active leaf.
fn merge_live_steps(
  rows: List(TreeRow),
  steps: List(client.Step),
) -> List(TreeRow) {
  let idx = index_where(rows, fn(r) { r.active })
  case idx >= 0, list_at(rows, idx) {
    True, Ok(active) -> {
      let live =
        list.map(steps, fn(s) {
          TreeRow(
            prefix: active.prefix,
            label: step_label(s),
            color: step_color(s),
            active: False,
            fork_id: "",
          )
        })
      let #(before, after) = list.split(rows, idx + 1)
      list.append(before, list.append(live, after))
    }
    _, _ -> rows
  }
}

/// Flatten the tree depth-first. Linear (single-child) runs keep a straight
/// gutter; only real fork points draw `├─`/`└─` connectors and indent.
fn build_tree_rows(tree: client.Tree) -> List(TreeRow) {
  case children_of(tree.entries, "") {
    [root] -> emit_node(tree.entries, root, "", "", tree.active_leaf)
    roots -> emit_siblings(tree.entries, roots, "", tree.active_leaf)
  }
}

fn emit_node(
  entries: List(client.TreeEntry),
  node: client.TreeEntry,
  gutter: String,
  connector: String,
  active_leaf: String,
) -> List(TreeRow) {
  let row =
    TreeRow(
      prefix: gutter <> connector,
      label: entry_label(node),
      color: entry_color(node),
      active: node.id == active_leaf,
      fork_id: node.id,
    )
  let child_gutter = gutter <> continuation(connector)
  let sub = case children_of(entries, node.id) {
    [] -> []
    // Linear run: stay straight (no connector, no extra indent).
    [only] -> emit_node(entries, only, child_gutter, "", active_leaf)
    // Real fork: indent each branch with a connector.
    kids -> emit_siblings(entries, kids, child_gutter, active_leaf)
  }
  [row, ..sub]
}

fn emit_siblings(
  entries: List(client.TreeEntry),
  kids: List(client.TreeEntry),
  gutter: String,
  active_leaf: String,
) -> List(TreeRow) {
  let n = list.length(kids)
  kids
  |> list.index_map(fn(k, i) {
    let connector = case i == n - 1 {
      True -> "└─ "
      False -> "├─ "
    }
    emit_node(entries, k, gutter, connector, active_leaf)
  })
  |> list.flatten
}

fn continuation(connector: String) -> String {
  case connector {
    "├─ " -> "│  "
    "└─ " -> "   "
    // Straight run: no added indent.
    _ -> ""
  }
}

fn children_of(
  entries: List(client.TreeEntry),
  parent_id: String,
) -> List(client.TreeEntry) {
  list.filter(entries, fn(e) { e.parent_id == parent_id })
}

/// Per-role / per-step color for a tree entry.
fn entry_color(e: client.TreeEntry) -> Color {
  case e.role {
    "user" -> style.Cyan
    "assistant" -> style.Green
    "tool_result" ->
      case client.decode_step(e.content) {
        Ok(step) -> step_color(step)
        Error(_) -> style.Grey
      }
    _ -> style.Grey
  }
}

fn step_color(step: client.Step) -> Color {
  case step {
    // Supervisor narration: de-emphasized.
    client.Text(_) | client.Plan(_) -> style.Grey
    // Tool invocations.
    client.Call(_, _) | client.ToolCall(_, _) -> style.Yellow
    // Results: green on success, red on failure.
    client.Exec(_, exit, _) | client.Worker(_, exit) ->
      case exit == 0 {
        True -> style.Green
        False -> style.Red
      }
    client.ToolResult(_, _) -> style.Green
    client.Check(ok, _) ->
      case ok {
        True -> style.Green
        False -> style.Red
      }
    client.Review(_) -> style.Magenta
  }
}

/// A pi-mono-style label for a tree entry: role-tagged text, or a compact
/// `[tool: arg]` for persisted run steps (`tool_result` role).
fn entry_label(e: client.TreeEntry) -> String {
  case e.role {
    "user" -> "user: " <> oneline(e.content)
    "assistant" ->
      case string.trim(e.content) {
        "" -> "assistant: (no content)"
        t -> "assistant: " <> oneline(t)
      }
    "tool_result" ->
      case client.decode_step(e.content) {
        Ok(step) -> step_label(step)
        Error(_) -> oneline(e.content)
      }
    other -> other <> ": " <> oneline(e.content)
  }
}

fn step_label(step: client.Step) -> String {
  case step {
    client.Text(t) -> oneline(t)
    client.Plan(t) -> oneline(t)
    client.Call(verb, arg) ->
      "[" <> string.lowercase(verb) <> ": " <> oneline(arg) <> "]"
    client.Exec(_verb, exit, _digest) -> "↳ exit " <> int.to_string(exit)
    client.Worker(cmd, exit) ->
      "[worker: " <> oneline(cmd) <> "] exit " <> int.to_string(exit)
    client.Check(ok, _digest) ->
      case ok {
        True -> "[check: ok]"
        False -> "[check: fail]"
      }
    client.Review(note) -> "[review: " <> oneline(note) <> "]"
    client.ToolCall(name, input) ->
      "[" <> name <> ": " <> oneline(input) <> "]"
    client.ToolResult(_name, output) -> "↳ " <> oneline(output)
  }
}

fn oneline(s: String) -> String {
  s
  |> string.replace("\n", " ")
  |> string.replace("\t", " ")
  |> string.trim
}

fn index_where(items: List(a), pred: fn(a) -> Bool) -> Int {
  index_where_loop(items, pred, 0)
}

fn index_where_loop(items: List(a), pred: fn(a) -> Bool, i: Int) -> Int {
  case items {
    [] -> -1
    [x, ..rest] ->
      case pred(x) {
        True -> i
        False -> index_where_loop(rest, pred, i + 1)
      }
  }
}

/// Rebuild the chat view (newest-first) from a tree's active branch.
/// Rebuild the chat (newest-first) from a tree's active branch. A turn's
/// `tool_result` steps and its assistant text are grouped into one `Bough`, so
/// jumping to a tool/worker node restores exactly the content shown at that
/// point — the steps up to and including that node.
fn chat_from_tree(tree: client.Tree) -> List(Entry) {
  let #(entries, buffer) =
    branch_path(tree)
    |> list.fold(#([], []), fn(acc, e) {
      let #(entries, buffer) = acc
      case e.role {
        // A user message closes the previous turn's step group.
        "user" -> #([You(e.content), ..flush_steps(buffer, entries)], [])
        "assistant" -> #(entries, [client.Text(e.content), ..buffer])
        "tool_result" ->
          case client.decode_step(e.content) {
            Ok(step) -> #(entries, [step, ..buffer])
            Error(_) -> #(entries, buffer)
          }
        _ -> #(entries, buffer)
      }
    })
  flush_steps(buffer, entries)
}

/// Prepend a turn's accumulated steps (newest-first) as one `Bough` entry.
fn flush_steps(buffer: List(client.Step), entries: List(Entry)) -> List(Entry) {
  case buffer {
    [] -> entries
    steps -> [Bough(list.reverse(steps)), ..entries]
  }
}

fn submit(model: Model) -> #(Model, List(fn() -> Msg)) {
  // An active `@` completion: Enter accepts it instead of sending.
  case model.suggestions {
    [top, ..] -> {
      let completed = accept_completion(model.input, top)
      #(
        Model(
          ..model,
          input: completed,
          cursor: string.length(completed),
          suggestions: [],
        ),
        [],
      )
    }
    [] ->
      case model.session, string.trim(model.input) {
        Some(id), text if text != "" -> {
          let model =
            Model(
              ..model,
              input: "",
              cursor: 0,
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
  let model = Model(..model, context_tokens: run.context_tokens)
  case run.status {
    "done" -> #(
      Model(
        ..model,
        status: "ready",
        pending: False,
        running_steps: [],
        chat: [Bough(run.steps), ..model.chat],
      ),
      // If the tree overlay is open, reload it now that the turn is persisted.
      tree_reload_effect(model),
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
        // While the tree overlay is open, fold the in-progress steps into it
        // live (they aren't persisted until the turn finishes).
        Some(id) -> #(
          rebuild_tree(Model(..model, running_steps: run.steps)),
          [poll(model.server, id)],
        )
        None -> #(rebuild_tree(Model(..model, running_steps: run.steps)), [])
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
        // Any keypress dismisses a stale selection highlight.
        _ -> on_key(clear_sel(model), ke)
      }
    event.Mouse(me) -> on_mouse(model, me)
  }
}

fn clear_sel(model: Model) -> Model {
  Model(..model, mouse_sel: None, autoscroll: 0)
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
      case model.view {
        ChatV ->
          case model.focused {
            True -> on_key_typing(model, ke)
            False -> on_key_command(model, ke)
          }
        _ -> on_key_overlay(model, ke)
      }
  }
}

fn on_key_overlay(model: Model, ke: event.KeyEvent) -> #(Model, List(fn() -> Msg)) {
  case ke.code {
    event.Esc -> update(model, CloseOverlay)
    event.UpArrow | event.Char("k") -> update(model, PickMove(-1))
    event.DownArrow | event.Char("j") -> update(model, PickMove(1))
    event.Enter -> update(model, PickChoose)
    _ -> #(model, [])
  }
}

fn on_key_typing(
  model: Model,
  ke: event.KeyEvent,
) -> #(Model, List(fn() -> Msg)) {
  let len = string.length(model.input)
  case ke.code {
    event.Enter -> update(model, Submit)
    event.Esc -> update(model, SetFocus(False))
    // Cmd+Left / Cmd+Right jump to the start / end of the line.
    event.LeftArrow if ke.modifiers.super -> #(Model(..model, cursor: 0), [])
    event.RightArrow if ke.modifiers.super -> #(Model(..model, cursor: len), [])
    event.LeftArrow -> #(Model(..model, cursor: int.max(0, model.cursor - 1)), [])
    event.RightArrow -> #(Model(..model, cursor: int.min(len, model.cursor + 1)), [])
    event.Home -> #(Model(..model, cursor: 0), [])
    event.End -> #(Model(..model, cursor: len), [])
    // Cmd+Backspace deletes from the caret back to the start of the line.
    event.Backspace if ke.modifiers.super ->
      update(model, InputChanged(string.drop_start(model.input, model.cursor), 0))
    event.Backspace ->
      case model.cursor > 0 {
        True -> {
          let before = string.slice(model.input, 0, model.cursor - 1)
          let after = string.drop_start(model.input, model.cursor)
          update(model, InputChanged(before <> after, model.cursor - 1))
        }
        False -> #(model, [])
      }
    event.Char(c) ->
      case printable(c) {
        True -> {
          let before = string.slice(model.input, 0, model.cursor)
          let after = string.drop_start(model.input, model.cursor)
          update(model, InputChanged(before <> c <> after, model.cursor + 1))
        }
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
    event.Char("s") -> update(model, OpenSessions)
    event.Char("t") -> update(model, OpenTree)
    _ -> #(model, [])
  }
}

fn on_mouse(
  model: Model,
  me: event.MouseEvent,
) -> #(Model, List(fn() -> Msg)) {
  case model.view, me.kind {
    ChatV, event.ScrollUp -> update(clear_sel(model), ScrollBy(3))
    ChatV, event.ScrollDown -> update(clear_sel(model), ScrollBy(-3))
    ChatV, event.Down(event.Left) -> on_mouse_down(model, me.column, me.row)
    ChatV, event.Drag(event.Left) -> on_mouse_drag(model, me.column, me.row)
    ChatV, event.Up(event.Left) -> on_mouse_up(model)
    _, event.ScrollUp -> update(model, PickMove(-1))
    _, event.ScrollDown -> update(model, PickMove(1))
    _, _ -> #(model, [])
  }
}

/// Pressing inside the conversation's text area begins a selection (resolved on
/// release); a press anywhere else keeps the old click-to-focus/toggle path.
fn on_mouse_down(model: Model, col: Int, row: Int) -> #(Model, List(fn() -> Msg)) {
  case in_conv_interior(model, col, row) {
    True ->
      case row_to_line(model, row) {
        Some(line) -> #(Model(..model, mouse_sel: Some(Region(line, col, line, col))), [])
        // Pressed on a "⋯ N above/below" marker row: not selectable text.
        None -> #(model, [])
      }
    False -> on_click(clear_sel(model), col, row)
  }
}

/// Extending a selection. While the pointer is inside the pane the head tracks
/// the line under it; past the top/bottom edge the view scrolls (further out =
/// faster) and the head sticks to the newly exposed edge line, so the selection
/// grows into content that was off-screen.
fn on_mouse_drag(model: Model, col: Int, row: Int) -> #(Model, List(fn() -> Msg)) {
  case model.mouse_sel {
    None -> #(model, [])
    Some(r) -> {
      let #(_cols, _rows, conv_w, conv_h) = dims(model)
      let last_row = conv_h - 2
      let was = model.autoscroll
      // Past an edge sets an autoscroll direction (signed lines per tick); the
      // timer loop then keeps scrolling while the button is held still.
      let #(head_line, head_col, dir) = case row <= 0, row >= conv_h - 1 {
        // Above the top edge: scroll toward older content, select to line start.
        True, _ -> #(top_visible_line(model), 2, edge_step(0 - row))
        // Below the bottom edge: scroll toward newer content, select to line end.
        _, True -> #(bottom_visible_line(model), conv_w - 3, 0 - edge_step(row - last_row))
        // Inside the pane: head follows the line under the cursor.
        _, _ -> #(line_under_row(model, row), int.clamp(col, 2, conv_w - 3), 0)
      }
      let model =
        Model(
          ..model,
          autoscroll: dir,
          mouse_sel: Some(Region(r.anchor_line, r.anchor_col, head_line, head_col)),
        )
      // Start the timer loop only on the 0 → active transition.
      let effects = case was == 0 && dir != 0 {
        True -> [autoscroll_tick]
        False -> []
      }
      #(model, effects)
    }
  }
}

/// Lines to scroll per tick for a drag `d` cells past an edge: at least 1,
/// ramping up so pushing further scrolls faster.
fn edge_step(d: Int) -> Int {
  int.clamp(d, 1, 6)
}

fn autoscroll_tick() -> Msg {
  process.sleep(30)
  AutoScroll
}

/// Releasing finishes a drag: an empty region was really a click (toggle/focus);
/// a real region is copied to the system clipboard and left highlighted.
fn on_mouse_up(model: Model) -> #(Model, List(fn() -> Msg)) {
  case model.mouse_sel {
    None -> #(model, [])
    Some(r) ->
      case is_empty_region(r) {
        True -> {
          // No drag: this was a click. Map the anchor line back to its screen
          // row for the toggle/focus handler.
          let row = line_to_row(model, r.anchor_line) |> option.unwrap(r.anchor_line)
          on_click(clear_sel(model), r.anchor_col, row)
        }
        False ->
          case selection_text(model, r) {
            "" -> #(clear_sel(model), [])
            text -> #(
              Model(
                ..model,
                autoscroll: 0,
                status: "copied " <> int.to_string(string.length(text)) <> " chars",
              ),
              [copy_effect(text)],
            )
          }
      }
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

// --- Mouse selection → clipboard ------------------------------------------

/// True when `col`/`row` fall on the conversation pane's text cells (inside the
/// border, left of the scrollbar). Matches `render_chat`'s `draw(2, row, …)`.
fn in_conv_interior(model: Model, col: Int, row: Int) -> Bool {
  let #(_cols, _rows, conv_w, conv_h) = dims(model)
  col >= 2 && col <= conv_w - 3 && row >= 1 && row <= conv_h - 2
}

fn is_empty_region(r: Region) -> Bool {
  r.anchor_line == r.head_line && r.anchor_col == r.head_col
}

/// Anchor/head ordered into reading order: `#(top_line, top_col, bot_line, bot_col)`.
fn ordered(r: Region) -> #(Int, Int, Int, Int) {
  case
    r.anchor_line < r.head_line
    || { r.anchor_line == r.head_line && r.anchor_col <= r.head_col }
  {
    True -> #(r.anchor_line, r.anchor_col, r.head_line, r.head_col)
    False -> #(r.head_line, r.head_col, r.anchor_line, r.anchor_col)
  }
}

// --- Screen-row ↔ transcript-line mapping ---------------------------------
// `window_bounds` gives the visible content slice `[start, start+inner)`;
// `scroll_window` prepends a "⋯ above" marker row when `start > 0`. These
// helpers convert between absolute transcript indices and on-screen rows
// through that same layout, so selection survives scrolling.

fn top_marker(model: Model) -> Int {
  let #(start, _inner, _total) = window_bounds(model)
  bool_int(start > 0)
}

/// The transcript line shown at screen `row`, if that row carries content (not a
/// marker / blank).
fn row_to_line(model: Model, row: Int) -> Option(Int) {
  let #(start, inner, total) = window_bounds(model)
  let j = start + row - 1 - top_marker(model)
  case j >= start && j < start + inner && j < total {
    True -> Some(j)
    False -> None
  }
}

/// Screen row showing transcript line `j`, if it is currently visible.
fn line_to_row(model: Model, j: Int) -> Option(Int) {
  let #(start, inner, _total) = window_bounds(model)
  case j >= start && j < start + inner {
    True -> Some(top_marker(model) + j - start + 1)
    False -> None
  }
}

/// Nearest selectable line for a row inside the pane, clamped to the visible
/// window (used while dragging across marker rows).
fn line_under_row(model: Model, row: Int) -> Int {
  case row_to_line(model, row) {
    Some(j) -> j
    None ->
      case row <= 1 {
        True -> top_visible_line(model)
        False -> bottom_visible_line(model)
      }
  }
}

fn top_visible_line(model: Model) -> Int {
  let #(start, _inner, _total) = window_bounds(model)
  start
}

fn bottom_visible_line(model: Model) -> Int {
  let #(start, inner, total) = window_bounds(model)
  int.min(start + inner - 1, total - 1)
}

/// The full transcript as displayed text per line (same truncation as
/// `render_chat`), indexed by absolute transcript line.
fn transcript_texts(model: Model) -> List(String) {
  let #(_cols, _rows, conv_w, _conv_h) = dims(model)
  transcript(model) |> list.map(fn(cl) { truncate(cl.text, conv_w - 4) })
}

/// Per-line selected pieces as `#(line_index, start_col, segment)`, covering the
/// whole selection — including lines currently scrolled off-screen.
fn selection_pieces(model: Model, r: Region) -> List(#(Int, Int, String)) {
  let #(tl, tc, bl, bc) = ordered(r)
  let texts = transcript_texts(model)
  int_range(tl, bl)
  |> list.filter_map(fn(j) {
    let text = nth(texts, j) |> result.unwrap("")
    let len = string.length(text)
    // Screen column → character index is `col - 2` (text is drawn at x = 2);
    // the head cell is inclusive, so the end index is `bc - 2 + 1`.
    let start = case j == tl {
      True -> tc - 2
      False -> 0
    }
    let end = case j == bl {
      True -> bc - 1
      False -> len
    }
    let s = int.clamp(start, 0, len)
    let e = int.clamp(end, s, len)
    case string.slice(text, s, e - s) {
      "" -> Error(Nil)
      seg -> Ok(#(j, s, seg))
    }
  })
}

fn nth(items: List(a), i: Int) -> Result(a, Nil) {
  list.drop(items, i) |> list.first
}

/// Inclusive ascending range `[from, to]` (stdlib `list.range` is unavailable
/// in this version).
fn int_range(from: Int, to: Int) -> List(Int) {
  case from > to {
    True -> []
    False -> [from, ..int_range(from + 1, to)]
  }
}

fn selection_text(model: Model, r: Region) -> String {
  selection_pieces(model, r)
  |> list.map(fn(piece) {
    let #(_line, _start, text) = piece
    text
  })
  |> string.join("\n")
}

fn copy_effect(text: String) -> fn() -> Msg {
  fn() {
    stdout.execute([command.Print(osc52(text))])
    Noop
  }
}

/// OSC 52 clipboard write: works in iTerm2, kitty, WezTerm, Ghostty and tmux
/// (with `set-clipboard on`); macOS Terminal.app ignores it.
fn osc52(text: String) -> String {
  let encoded = bit_array.base64_encode(bit_array.from_string(text), True)
  "\u{1b}]52;c;" <> encoded <> "\u{07}"
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
  CLine(
    text: String,
    color: Color,
    attrs: List(Attribute),
    click: Option(Msg),
    // Glyphs drawn just after the line's text, each in its own color (e.g. a
    // round's check/review markers on the plan line). Empty for normal lines.
    markers: List(#(String, Color)),
  )
}

fn line(text: String, color: Color) -> CLine {
  CLine(text, color, [], None, [])
}

/// A bold line (used for turn headers and markdown headings).
fn bold(text: String, color: Color) -> CLine {
  CLine(text, color, [style.Bold], None, [])
}

/// A dim, low-emphasis line (tool metadata, hints, chrome).
fn dim(text: String, color: Color) -> CLine {
  CLine(text, color, [style.Dim], None, [])
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
        [bold("▌ you", style.Cyan), line("", style.Default)],
        wrap_styled(text, width, style.Default),
        [line("", style.Default)],
      ]),
      idx,
    )
    Failed(e) -> #(
      list.flatten([
        [bold("▌ error", style.Red), line("", style.Default)],
        wrap_styled(e, width, style.Red),
        [line("", style.Default)],
      ]),
      idx,
    )
    Bough(steps) -> {
      // A round's passing CHECK and review-requested ride as right-margin
      // glyphs on its opening sentence instead of taking their own lines.
      let #(marks, hidden) = associate_markers(steps)
      let #(step_lines, idx2, _pos) =
        list.fold(steps, #([], idx, 0), fn(acc, step) {
          let #(ls, i, p) = acc
          let #(more, i2) = case step {
            Plan(text) ->
              case dict.get(marks, p) {
                Ok(flags) -> #(plan_with_marker(text, width, flags), i)
                Error(_) -> render_step(step, width, model, i)
              }
            Check(True, _) ->
              case set.contains(hidden, p) {
                True -> #([], i + 1)
                False -> render_step(step, width, model, i)
              }
            Review(_) ->
              case set.contains(hidden, p) {
                True -> #([], i)
                False -> render_step(step, width, model, i)
              }
            _ -> render_step(step, width, model, i)
          }
          #(list.append(ls, more), i2, p + 1)
        })
      #(
        list.flatten([
          [bold("▌ bough", style.Green), line("", style.Default)],
          step_lines,
          [line("", style.Default)],
        ]),
        idx2,
      )
    }
  }
}

/// For each round, fold its passing CHECK and review-requested onto the plan
/// that opened it. Returns per-plan marker flags `#(checked, reviewed)` and the
/// positions of the check/review steps to hide (now shown as margin glyphs).
fn associate_markers(
  steps: List(Step),
) -> #(Dict(Int, #(Bool, Bool)), Set(Int)) {
  let #(marks, hidden, _last, _pos) =
    list.fold(steps, #(dict.new(), set.new(), None, 0), fn(acc, step) {
      let #(marks, hidden, last_plan, pos) = acc
      case step, last_plan {
        Plan(_), _ -> #(marks, hidden, Some(pos), pos + 1)
        Check(True, _), Some(pp) -> #(
          mark(marks, pp, True, False),
          set.insert(hidden, pos),
          last_plan,
          pos + 1,
        )
        Review(note), Some(pp) ->
          case string.starts_with(note, "requested") {
            True -> #(
              mark(marks, pp, False, True),
              set.insert(hidden, pos),
              last_plan,
              pos + 1,
            )
            False -> #(marks, hidden, last_plan, pos + 1)
          }
        _, _ -> #(marks, hidden, last_plan, pos + 1)
      }
    })
  #(marks, hidden)
}

fn mark(
  marks: Dict(Int, #(Bool, Bool)),
  pp: Int,
  check: Bool,
  review: Bool,
) -> Dict(Int, #(Bool, Bool)) {
  let #(c, r) = dict.get(marks, pp) |> result.unwrap(#(False, False))
  dict.insert(marks, pp, #(c || check, r || review))
}

/// The plan prose with its round's status as glyphs right after the first
/// line's text: a green check if the check passed, a magenta wave if a review
/// was requested.
fn plan_with_marker(
  text: String,
  width: Int,
  flags: #(Bool, Bool),
) -> List(CLine) {
  let #(checked, reviewed) = flags
  let markers =
    list.flatten([
      case checked {
        True -> [#(glyph_check, style.Green)]
        False -> []
      },
      case reviewed {
        True -> [#(glyph_wave, style.Magenta)]
        False -> []
      },
    ])
  case render_markdown(text, width) {
    [first, ..rest] -> [
      CLine(first.text, first.color, first.attrs, first.click, markers),
      ..rest
    ]
    [] -> [CLine("", style.Default, [], None, markers)]
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
    ToolCall(name, input) -> #([call_line(format_tool_call(name, input))], idx)
    ToolResult(_name, output) -> #(render_result(output, width, model, idx, False), idx + 1)
    // --- supervisor-worker phased events (SPEC §5) ---
    Plan(text) -> #(render_markdown(text, width), idx)
    // READ/GREP are the agent inspecting the workspace — output for the
    // supervisor, not the viewer — so they're hidden from the timeline. Press
    // `o` (expand-all) to reveal them.
    Call(verb, arg) ->
      case is_introspection(verb) && !model.expand_all {
        True -> #([], idx)
        False -> #([call_line(verb <> "  " <> arg)], idx)
      }
    // A successful step needs no status line — the result (or its absence) says
    // so. Only a failure gets a loud line, and its output stays expanded.
    Exec(verb, exit, digest) ->
      case is_introspection(verb) && !model.expand_all {
        True -> #([], idx + 1)
        False -> {
          let status = case exit == 0 {
            True -> []
            False -> [line("  ✗ exit " <> int.to_string(exit), style.Red)]
          }
          #(
            list.append(status, render_result(digest, width, model, idx, exit != 0)),
            idx + 1,
          )
        }
      }
    Worker(command, exit) -> #([worker_line(command, exit)], idx)
    Check(ok, digest) -> {
      let status = case ok {
        True -> line(glyph_check, style.Green)
        False -> line("✗ CHECK failed", style.Red)
      }
      #([status, ..render_result(digest, width, model, idx, !ok)], idx + 1)
    }
    Review(note) -> #(wrap_styled("◆ review — " <> note, width, style.Magenta), idx)
  }
}

/// Read-only inspection steps, hidden from the timeline by default.
fn is_introspection(verb: String) -> Bool {
  verb == "READ" || verb == "GREP"
}

/// A harness action line (READ/RUN/WRITE/…): legible grey — not dim — so the
/// verb and path stay scannable, but quieter than the prose answer. The leading
/// ↳ marks it as a sub-step of the turn.
fn call_line(text: String) -> CLine {
  line("↳ " <> text, style.Grey)
}

/// A local-worker fix attempt and its outcome.
fn worker_line(command: String, exit: Int) -> CLine {
  let status = case exit == 0 {
    True -> "  ✓"
    False -> "  ✗ exit " <> int.to_string(exit)
  }
  line("  ↺ worker  " <> command <> status, style.Yellow)
}

/// A step's output, collapsed to a one-line `▸ N lines` toggle by default
/// (`force` or a manual/expand-all toggle shows it in full). Empty output
/// renders nothing — the status line above already says it succeeded.
fn render_result(
  output: String,
  width: Int,
  model: Model,
  idx: Int,
  force: Bool,
) -> List(CLine) {
  let expanded = force || model.expand_all || set.contains(model.expanded, idx)
  let lines = case string.trim_end(output) {
    "" -> []
    trimmed -> string.split(trimmed, "\n")
  }
  let total = list.length(lines)
  let body = case total {
    0 -> []
    _ ->
      case expanded {
        True -> {
          let railed = list.map(lines, fn(l) { rail(truncate(l, width - 2)) })
          list.append(railed, [toggle("  ▾ collapse", idx)])
        }
        False -> {
          let noun = case total {
            1 -> " line"
            _ -> " lines"
          }
          [toggle("  ▸ " <> int.to_string(total) <> noun, idx)]
        }
      }
  }
  list.append(body, [line("", style.Default)])
}

fn toggle(label: String, idx: Int) -> CLine {
  CLine(label, style.Cyan, [], Some(ToggleResult(idx)), [])
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

/// The visible window over the transcript as `#(start, inner, total)`, matching
/// scroll_window's math so the scrollbar agrees with the "N above/below" markers.
fn window_bounds(model: Model) -> #(Int, Int, Int) {
  let total = list.length(transcript(model))
  let budget = conv_inner_h(model)
  case total <= budget {
    True -> #(0, total, total)
    False -> {
      let scrolled = model.scroll > 0
      let inner = int.max(budget - 1 - bool_int(scrolled), 1)
      let max_scroll = total - inner
      let s = int.clamp(model.scroll, 0, max_scroll)
      let start = int.max(total - inner - s, 0)
      #(start, inner, total)
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
  case model.view {
    ChatV -> render_chat(model)
    SessionsV ->
      render_overlay(
        model,
        "resume session  ·  ↑↓ select · Enter open · Esc cancel",
        list.map(model.sessions, fn(s) {
          let when = case s.turns {
            1 -> "1 turn"
            n -> int.to_string(n) <> " turns"
          }
          one_line(s.title <> "   (" <> when <> ")")
        }),
      )
    TreeV -> render_tree_overlay(model)
  }
}

/// The conversation-tree overlay: a colored, depth-first tree where linear runs
/// stay straight and only forks indent. Dim connector gutters, role/step-tinted
/// labels, and an `← active` marker on the current leaf.
fn render_tree_overlay(model: Model) -> List(Command) {
  let #(cols, rows, _conv_w, _conv_h) = dims(model)
  let title =
    "conversation tree  ·  ↑↓ select · Enter fork from here · Esc cancel"
  let inner_h = int.max(rows - 2, 1)
  let total = list.length(model.tree_rows)
  let start = case total > inner_h {
    True -> int.clamp(model.sel - inner_h / 2, 0, total - inner_h)
    False -> 0
  }
  let body =
    model.tree_rows
    |> list.drop(start)
    |> list.take(inner_h)
    |> list.index_map(fn(row, i) {
      let screen_row = i + 1
      let selected = start + i == model.sel
      // Selection caret.
      let caret = case selected {
        True -> draw(2, screen_row, "›", style.Cyan, [style.Bold])
        False -> []
      }
      // Dim connector gutter.
      let gutter = draw(4, screen_row, row.prefix, style.Grey, [style.Dim])
      let lx = 4 + string.length(row.prefix)
      let marker = case row.active {
        True -> " ← active"
        False -> ""
      }
      let bold = selected || row.active
      let avail = int.max(cols - lx - 2 - string.length(marker), 1)
      let label = truncate(row.label, avail)
      let label_cmds = draw_label(lx, screen_row, label, row.color, bold)
      let active_cmds = case row.active {
        True ->
          draw(lx + string.length(label), screen_row, marker, style.Yellow, [
            style.Bold,
          ])
        False -> []
      }
      list.flatten([caret, gutter, label_cmds, active_cmds])
    })
    |> list.flatten
  let empty = case total {
    0 -> draw(2, 1, "(no history yet)", style.Grey, [style.Dim])
    _ -> []
  }
  list.flatten([
    [command.Clear(terminal.All)],
    box(0, 0, cols, rows, title, style.Cyan),
    body,
    empty,
    [command.HideCursor],
  ])
}

fn one_line(text: String) -> String {
  text
}

/// A full-screen list picker (resume / branch), with the selected row arrowed.
fn render_overlay(
  model: Model,
  title: String,
  items: List(String),
) -> List(Command) {
  let #(cols, rows, _conv_w, _conv_h) = dims(model)
  let inner_h = int.max(rows - 2, 1)
  let total = list.length(items)
  let start = case total > inner_h {
    True -> int.clamp(model.sel - inner_h / 2, 0, total - inner_h)
    False -> 0
  }
  let visible = items |> list.drop(start) |> list.take(inner_h)
  let body =
    visible
    |> list.index_map(fn(text, i) {
      let selected = start + i == model.sel
      let prefix = case selected {
        True -> "› "
        False -> "  "
      }
      case selected {
        True -> draw(2, i + 1, truncate(prefix <> text, cols - 3), style.Cyan, [style.Bold])
        False -> draw(2, i + 1, truncate(prefix <> text, cols - 3), style.Default, [])
      }
    })
    |> list.flatten
  let empty = case total {
    0 -> draw(2, 1, "(no sessions yet)", style.Grey, [style.Dim])
    _ -> []
  }
  list.flatten([
    [command.Clear(terminal.All)],
    box(0, 0, cols, rows, title, style.Cyan),
    body,
    empty,
    [command.HideCursor],
  ])
}

/// Draw colored text. etch's `SetStyle` with empty attributes emits a trailing
/// `;m` that resets the color, so plain (non-bold) labels use `SetForegroundColor`
/// (a clean `CSI <fg>m`); bold labels already carry an attribute, so `SetStyle`
/// is safe there.
fn draw_label(
  x: Int,
  y: Int,
  text: String,
  color: Color,
  bold: Bool,
) -> List(Command) {
  case bold {
    True -> draw(x, y, text, color, [style.Bold])
    False -> [
      command.MoveTo(x, y),
      command.SetForegroundColor(color),
      command.Print(text),
      command.ResetStyle,
    ]
  }
}

fn render_chat(model: Model) -> List(Command) {
  let #(cols, rows, conv_w, conv_h) = dims(model)
  let net_x = conv_w
  let net_w = cols - conv_w

  let convo =
    visible_conversation(model)
    |> list.flat_map(fn(pair) {
      let #(row, cl) = pair
      // Leave the last interior column for the right-hand scrollbar.
      case cl.markers {
        [] -> draw(2, row, truncate(cl.text, conv_w - 4), cl.color, cl.attrs)
        markers -> {
          // Glyphs sit just after the text (each +1 for a leading space), so
          // reserve that much room when truncating, then draw them in sequence.
          let span =
            list.fold(markers, 0, fn(n, m) { n + string.length(m.0) + 1 })
          let text = truncate(cl.text, conv_w - 4 - span)
          let base = draw(2, row, text, cl.color, cl.attrs)
          let #(marker_cmds, _) =
            list.fold(markers, #([], 2 + string.length(text) + 1), fn(acc, m) {
              let #(cmds, col) = acc
              let #(glyph, color) = m
              #(
                list.append(cmds, draw(col, row, glyph, color, [])),
                col + string.length(glyph) + 1,
              )
            })
          list.append(base, marker_cmds)
        }
      }
    })

  list.flatten([
    [command.Clear(terminal.All)],
    box(0, 0, conv_w, conv_h, "conversation", style.Blue),
    convo,
    selection_overlay(model),
    scrollbar(model),
    box(net_x, 0, net_w, conv_h, "network", style.Magenta),
    network_panel(model, net_x + 2, net_w - 3),
    box(0, conv_h, cols, 3, input_title(model), style.Cyan),
    input_panel(model, conv_h, cols),
    status_line(model, rows - 1, cols),
    cursor(model, conv_h, cols),
  ])
}

/// A vertical scrollbar just inside the conversation's right border: a dim track
/// the height of the viewport with a brighter thumb sized and positioned to the
/// visible window. Hidden when everything already fits.
fn scrollbar(model: Model) -> List(Command) {
  let #(_cols, _rows, conv_w, _conv_h) = dims(model)
  let x = conv_w - 2
  let track = conv_inner_h(model)
  let #(start, inner, total) = window_bounds(model)
  case total <= track {
    True -> []
    False -> {
      // Map the visible window [start, start+inner) of `total` onto the track.
      let thumb_h = int.clamp(track * inner / total, 1, track)
      let thumb_top = int.clamp(track * start / total, 0, track - thumb_h)
      list.repeat(Nil, track)
      |> list.index_map(fn(_, i) {
        case i >= thumb_top && i < thumb_top + thumb_h {
          True -> draw(x, i + 1, "█", style.Grey, [])
          False -> draw(x, i + 1, "░", style.Grey, [style.Dim])
        }
      })
      |> list.flatten
    }
  }
}

fn input_title(model: Model) -> String {
  case model.focused {
    True -> "message"
    False -> "message — i/Enter to type"
  }
}

/// The slice of `input` that fits the input box and the caret's column within
/// it. The window scrolls so the caret stays visible when the text overflows.
fn input_view(model: Model, avail: Int) -> #(String, Int) {
  let len = string.length(model.input)
  case len <= avail {
    True -> #(model.input, model.cursor)
    False -> {
      let start = int.clamp(model.cursor - avail, 0, len - avail)
      #(string.slice(model.input, start, avail), model.cursor - start)
    }
  }
}

fn input_panel(model: Model, conv_h: Int, cols: Int) -> List(Command) {
  let avail = cols - 5
  let #(shown, _) = input_view(model, avail)
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
      let #(_, col) = input_view(model, avail)
      [command.MoveTo(4 + col, conv_h + 1), command.ShowCursor]
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
          <> "  ·  ↑↓ scroll · drag copies · s resume · t branch · o expand · i type · Ctrl+X quit"
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
  // Right-aligned: the supervisor model, plus the context-usage meter once a
  // run has reported tokens.
  let right = right_status(model)
  let right_w = string.length(right)
  let meter = case right_w > 0 && right_w < cols {
    True -> draw(cols - right_w, row, right, style.Grey, [style.Dim])
    False -> []
  }
  list.flatten([
    draw(0, row, truncate(text, int.max(0, cols - right_w - 1)), color, attrs),
    meter,
  ])
}

/// The right-hand status segment: `<model> · ctx NN%`, omitting either part
/// that isn't known yet.
fn right_status(model: Model) -> String {
  case model.model_name, context_meter(model.context_tokens) {
    "", ctx -> ctx
    name, "" -> name
    name, ctx -> name <> " · " <> ctx
  }
}

const context_window = 200_000

/// "ctx NN%" for the status meter, or "" when no run has reported tokens yet.
fn context_meter(tokens: Int) -> String {
  case tokens > 0 {
    True ->
      "ctx " <> int.to_string(int.min(100, tokens * 100 / context_window)) <> "%"
    False -> ""
  }
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

/// Like `draw`, but with an explicit background — used to paint the selection.
fn draw_bg(
  x: Int,
  y: Int,
  text: String,
  fg: Color,
  bg: Color,
  attrs: List(Attribute),
) -> List(Command) {
  [
    command.MoveTo(x, y),
    command.SetStyle(style.Style(bg: bg, fg: fg, attributes: attrs)),
    command.Print(text),
    command.ResetStyle,
  ]
}

/// Repaints the selected text segments with a highlight background, on top of
/// the already-drawn conversation.
fn selection_overlay(model: Model) -> List(Command) {
  case model.mouse_sel {
    None -> []
    Some(r) ->
      selection_pieces(model, r)
      |> list.filter_map(fn(piece) {
        let #(line, start, text) = piece
        // Only paint pieces whose line is currently on-screen.
        case line_to_row(model, line) {
          Some(row) -> Ok(draw_bg(2 + start, row, text, style.White, style.Blue, []))
          None -> Error(Nil)
        }
      })
      |> list.flatten
  }
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
