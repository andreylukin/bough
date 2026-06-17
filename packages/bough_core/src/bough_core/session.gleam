//// The session tree: bough's branchable history.
////
//// Stored as JSONL under `~/.bough/sessions/<id>.jsonl`: the first line is a
//// metadata object, each subsequent line is one `Entry` (oldest first). Each
//// entry knows its parent, so the conversation is a tree; the `active_leaf`
//// marks the current position. Branching is appending an entry whose
//// `parent_id` points at an earlier node. See SPEC.md §4.
////
//// This module is pure: it (de)serializes but performs no IO. The server owns
//// id generation, timestamps, and disk access.

import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string

pub type Role {
  User
  Assistant
  ToolResult
  System
}

/// One node in the session tree.
///
/// `snapshot_ref`, when present, points at a nono rollback snapshot captured
/// for this node — that is what lets a fork restore the filesystem, not just
/// the chat (SPEC.md §4.1).
pub type Entry {
  Entry(
    id: String,
    parent_id: Option(String),
    role: Role,
    content: String,
    snapshot_ref: Option(String),
    label: Option(String),
    timestamp: Int,
  )
}

pub type SessionTree {
  SessionTree(
    id: String,
    project: String,
    entries: List(Entry),
    active_leaf: Option(String),
  )
}

pub fn new(id: String, project: String) -> SessionTree {
  SessionTree(id: id, project: project, entries: [], active_leaf: None)
}

/// Append an entry and move the active leaf to it.
pub fn append(tree: SessionTree, entry: Entry) -> SessionTree {
  SessionTree(
    ..tree,
    entries: [entry, ..tree.entries],
    active_leaf: Some(entry.id),
  )
}

/// Direct children of a node (`None` = roots).
pub fn children_of(tree: SessionTree, parent: Option(String)) -> List(Entry) {
  list.filter(tree.entries, fn(e) { e.parent_id == parent })
}

pub fn get(tree: SessionTree, id: String) -> Option(Entry) {
  tree.entries
  |> list.filter(fn(e) { e.id == id })
  |> list.first
  |> option.from_result
}

/// Move the active leaf to an existing node — the basis for `/tree` jumps and
/// `/fork` (SPEC.md §4).
pub fn set_leaf(tree: SessionTree, id: String) -> SessionTree {
  SessionTree(..tree, active_leaf: Some(id))
}

// --- Roles ---------------------------------------------------------------

pub fn role_to_string(role: Role) -> String {
  case role {
    User -> "user"
    Assistant -> "assistant"
    ToolResult -> "tool_result"
    System -> "system"
  }
}

pub fn role_from_string(s: String) -> Result(Role, Nil) {
  case s {
    "user" -> Ok(User)
    "assistant" -> Ok(Assistant)
    "tool_result" -> Ok(ToolResult)
    "system" -> Ok(System)
    _ -> Error(Nil)
  }
}

// --- JSON ----------------------------------------------------------------

pub fn entry_to_json(entry: Entry) -> json.Json {
  json.object([
    #("id", json.string(entry.id)),
    #("parent_id", json.nullable(entry.parent_id, json.string)),
    #("role", json.string(role_to_string(entry.role))),
    #("content", json.string(entry.content)),
    #("snapshot_ref", json.nullable(entry.snapshot_ref, json.string)),
    #("label", json.nullable(entry.label, json.string)),
    #("timestamp", json.int(entry.timestamp)),
  ])
}

pub fn entry_decoder() -> decode.Decoder(Entry) {
  use id <- decode.field("id", decode.string)
  use parent_id <- decode.field("parent_id", decode.optional(decode.string))
  use role_s <- decode.field("role", decode.string)
  use content <- decode.field("content", decode.string)
  use snapshot_ref <- decode.field(
    "snapshot_ref",
    decode.optional(decode.string),
  )
  use label <- decode.field("label", decode.optional(decode.string))
  use timestamp <- decode.field("timestamp", decode.int)
  case role_from_string(role_s) {
    Ok(role) ->
      decode.success(Entry(
        id: id,
        parent_id: parent_id,
        role: role,
        content: content,
        snapshot_ref: snapshot_ref,
        label: label,
        timestamp: timestamp,
      ))
    Error(_) ->
      decode.failure(
        Entry(id, None, User, content, None, None, timestamp),
        "Role",
      )
  }
}

/// Full tree as a JSON object (for API responses).
pub fn tree_to_json(tree: SessionTree) -> json.Json {
  json.object([
    #("id", json.string(tree.id)),
    #("project", json.string(tree.project)),
    #("active_leaf", json.nullable(tree.active_leaf, json.string)),
    #("entries", json.array(list.reverse(tree.entries), entry_to_json)),
  ])
}

// --- JSONL persistence ---------------------------------------------------

type Meta {
  Meta(id: String, project: String, active_leaf: Option(String))
}

fn meta_to_json(tree: SessionTree) -> json.Json {
  json.object([
    #("id", json.string(tree.id)),
    #("project", json.string(tree.project)),
    #("active_leaf", json.nullable(tree.active_leaf, json.string)),
  ])
}

fn meta_decoder() -> decode.Decoder(Meta) {
  use id <- decode.field("id", decode.string)
  use project <- decode.field("project", decode.string)
  use active_leaf <- decode.field("active_leaf", decode.optional(decode.string))
  decode.success(Meta(id: id, project: project, active_leaf: active_leaf))
}

/// Serialize a tree to JSONL: meta line followed by entries oldest-first.
pub fn to_jsonl(tree: SessionTree) -> String {
  let meta = json.to_string(meta_to_json(tree))
  let entries =
    tree.entries
    |> list.reverse
    |> list.map(fn(e) { json.to_string(entry_to_json(e)) })
  string.join([meta, ..entries], "\n")
}

/// Parse a tree from JSONL produced by `to_jsonl`.
pub fn from_jsonl(contents: String) -> Result(SessionTree, String) {
  let lines =
    contents
    |> string.split("\n")
    |> list.filter(fn(l) { l != "" })

  case lines {
    [] -> Error("empty session file")
    [meta_line, ..entry_lines] -> {
      use meta <- result.try(
        json.parse(meta_line, meta_decoder())
        |> result.replace_error("invalid meta line"),
      )
      use entries <- result.try(
        entry_lines
        |> list.try_map(fn(l) {
          json.parse(l, entry_decoder())
          |> result.replace_error("invalid entry line")
        }),
      )
      Ok(SessionTree(
        id: meta.id,
        project: meta.project,
        entries: list.reverse(entries),
        active_leaf: meta.active_leaf,
      ))
    }
  }
}
