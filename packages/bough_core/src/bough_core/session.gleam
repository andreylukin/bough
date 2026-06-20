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

import gleam/dict.{type Dict}
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
///
/// `grafted_from`, when present, marks this node as a graft copy and points at
/// the original it was copied from (SPEC.md §4.2). Graft copies carry no
/// `snapshot_ref`: a graft moves the conversation, not the files.
pub type Entry {
  Entry(
    id: String,
    parent_id: Option(String),
    role: Role,
    content: String,
    snapshot_ref: Option(String),
    label: Option(String),
    timestamp: Int,
    grafted_from: Option(String),
  )
}

pub type SessionTree {
  SessionTree(
    id: String,
    project: String,
    entries: List(Entry),
    active_leaf: Option(String),
    /// Hosts the human has approved for the agent's sandboxed commands — the
    /// network allowlist, which grows as requests are approved (SPEC.md §7).
    allow_domains: List(String),
    /// nono capability groups the human has enabled for this session, layered
    /// into the sandbox profile on top of the always-on base (SPEC.md §7).
    groups: List(String),
    /// The graft operations applied to this tree, newest first (SPEC.md §4.2).
    /// Each records which original nodes it superseded; that's what marks them
    /// hidden in the default view without ever deleting a line.
    grafts: List(GraftEvent),
  )
}

pub fn new(id: String, project: String) -> SessionTree {
  SessionTree(
    id: id,
    project: project,
    entries: [],
    active_leaf: None,
    allow_domains: [],
    groups: [],
    grafts: [],
  )
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

/// The active branch: entries from the root down to `active_leaf`, oldest
/// first. This is the conversation to replay for context (SPEC.md §4) — a fork
/// just repoints `active_leaf` at an earlier node, yielding a different path.
pub fn path(tree: SessionTree) -> List(Entry) {
  case tree.active_leaf {
    None -> []
    Some(leaf) -> walk_up(tree, leaf, [])
  }
}

fn walk_up(tree: SessionTree, id: String, acc: List(Entry)) -> List(Entry) {
  case get(tree, id) {
    None -> acc
    Some(e) -> {
      let acc = [e, ..acc]
      case e.parent_id {
        Some(parent) -> walk_up(tree, parent, acc)
        None -> acc
      }
    }
  }
}

/// Move the active leaf to an existing node — the basis for `/tree` jumps and
/// `/fork` (SPEC.md §4).
pub fn set_leaf(tree: SessionTree, id: String) -> SessionTree {
  SessionTree(..tree, active_leaf: Some(id))
}

/// The snapshot to restore when forking to node `id`: its own `snapshot_ref`, or
/// the nearest ancestor's if it has none (only completed turns carry one). Used
/// so a fork restores the filesystem to that node's state (SPEC.md §4.1).
pub fn nearest_snapshot(tree: SessionTree, id: String) -> Option(String) {
  case get(tree, id) {
    None -> None
    Some(e) ->
      case e.snapshot_ref {
        Some(_) -> e.snapshot_ref
        None ->
          case e.parent_id {
            Some(parent) -> nearest_snapshot(tree, parent)
            None -> None
          }
      }
  }
}

// --- Graft (SPEC.md §4.2) ------------------------------------------------

/// A graft moves the conversation, not the files: the agent rebuilds any work
/// against the real files on its next turn, so this marker is injected ahead of
/// the moved turns to stop it assuming that work is already present.
pub const graft_marker = "[grafted — prior files aren't present; current files are the base]"

/// One graft operation, persisted as its own JSONL record (not an `Entry`).
/// `mapping` is original id → new (copy) id; its keys are the superseded nodes.
pub type GraftEvent {
  GraftEvent(
    id: String,
    section_root: String,
    onto: String,
    mapping: Dict(String, String),
    timestamp: Int,
  )
}

pub type GraftError {
  /// `section_root` or `onto` is not a node in the tree.
  GraftNodeNotFound(String)
  /// `onto` is the section itself or one of its descendants — would cycle.
  GraftCycle
}

/// The product of planning a graft: the new entries to append (the marker
/// followed by the re-parented copies, parent-first), the event to record, and
/// the leaf to move to.
pub type Graft {
  Graft(entries: List(Entry), event: GraftEvent, new_leaf: String)
}

/// The original ids superseded by grafts — hidden from the default tree view.
pub fn superseded_ids(tree: SessionTree) -> List(String) {
  list.flat_map(tree.grafts, fn(g) { dict.keys(g.mapping) })
}

/// Plan a graft of the subtree rooted at `section_root` onto `onto`: validate it,
/// then produce copies of the section re-parented under `onto` (via an injected
/// marker), each stamped with `grafted_from` and carrying no snapshot. Pure —
/// the caller supplies a fresh, unique `salt` (the server uses a random string)
/// that prefixes every generated id, plus the timestamp (`now`) — and applies
/// the result with `apply_graft`. Originals are left untouched; the returned
/// `GraftEvent` is what marks them superseded.
pub fn plan_graft(
  tree: SessionTree,
  section_root: String,
  onto: String,
  salt: String,
  now: Int,
) -> Result(Graft, GraftError) {
  use _ <- result.try(
    get(tree, section_root) |> option.to_result(GraftNodeNotFound(section_root)),
  )
  use _ <- result.try(
    get(tree, onto) |> option.to_result(GraftNodeNotFound(onto)),
  )
  let section = subtree(tree, section_root)
  case list.any(section, fn(e) { e.id == onto }) {
    True -> Error(GraftCycle)
    False -> {
      // A node's copy id is the salt-prefixed original; the salt is fresh per
      // graft, so copies are unique even when re-grafting a graft.
      let new_id = fn(id) { salt <> "-" <> id }
      let marker_id = salt <> "-marker"
      let mapping =
        list.fold(section, dict.new(), fn(acc, e) {
          dict.insert(acc, e.id, new_id(e.id))
        })
      let copies =
        list.map(section, fn(e) {
          let parent = case e.id == section_root {
            True -> Some(marker_id)
            False -> Some(new_id(option.unwrap(e.parent_id, section_root)))
          }
          Entry(
            id: new_id(e.id),
            parent_id: parent,
            role: e.role,
            content: e.content,
            snapshot_ref: None,
            label: e.label,
            timestamp: e.timestamp,
            grafted_from: Some(e.id),
          )
        })
      let marker =
        Entry(
          id: marker_id,
          parent_id: Some(onto),
          role: User,
          content: graft_marker,
          snapshot_ref: None,
          label: None,
          timestamp: now,
          grafted_from: None,
        )
      let event =
        GraftEvent(
          id: salt <> "-graft",
          section_root: section_root,
          onto: onto,
          mapping: mapping,
          timestamp: now,
        )
      Ok(Graft(
        entries: [marker, ..copies],
        event: event,
        // Continue from the copy of the section's tip (follow first children),
        // so you pick up where the moved work left off.
        new_leaf: new_id(section_tip(tree, section_root)),
      ))
    }
  }
}

/// Apply a planned graft: append its entries, record the event, and move the
/// active leaf onto the grafted section. Append-only — nothing is removed.
pub fn apply_graft(tree: SessionTree, graft: Graft) -> SessionTree {
  SessionTree(
    ..tree,
    entries: list.append(list.reverse(graft.entries), tree.entries),
    grafts: [graft.event, ..tree.grafts],
    active_leaf: Some(graft.new_leaf),
  )
}

/// The subtree rooted at `id` (the node and all descendants), parent-first.
fn subtree(tree: SessionTree, id: String) -> List(Entry) {
  case get(tree, id) {
    None -> []
    Some(e) -> [
      e,
      ..list.flat_map(children_of(tree, Some(id)), fn(c) { subtree(tree, c.id) })
    ]
  }
}

/// The deepest node reached by following the first child from `id` — the tip of
/// the section in the common linear case.
fn section_tip(tree: SessionTree, id: String) -> String {
  case children_of(tree, Some(id)) {
    [] -> id
    [first, ..] -> section_tip(tree, first.id)
  }
}

fn graft_event_to_json(g: GraftEvent) -> json.Json {
  json.object([
    #("op", json.string("graft")),
    #("id", json.string(g.id)),
    #("section_root", json.string(g.section_root)),
    #("onto", json.string(g.onto)),
    #(
      "mapping",
      json.object(
        dict.to_list(g.mapping) |> list.map(fn(p) { #(p.0, json.string(p.1)) }),
      ),
    ),
    #("timestamp", json.int(g.timestamp)),
  ])
}

fn graft_event_decoder() -> decode.Decoder(GraftEvent) {
  use _ <- decode.field("op", decode.string)
  use id <- decode.field("id", decode.string)
  use section_root <- decode.field("section_root", decode.string)
  use onto <- decode.field("onto", decode.string)
  use mapping <- decode.field(
    "mapping",
    decode.dict(decode.string, decode.string),
  )
  use timestamp <- decode.field("timestamp", decode.int)
  decode.success(GraftEvent(
    id: id,
    section_root: section_root,
    onto: onto,
    mapping: mapping,
    timestamp: timestamp,
  ))
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
    #("grafted_from", json.nullable(entry.grafted_from, json.string)),
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
  // Older session files predate graft — default to a non-graft node.
  use grafted_from <- decode.optional_field(
    "grafted_from",
    None,
    decode.optional(decode.string),
  )
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
        grafted_from: grafted_from,
      ))
    Error(_) ->
      decode.failure(
        Entry(id, None, User, content, None, None, timestamp, None),
        "Role",
      )
  }
}

/// Full tree as a JSON object (for API responses). `grafts` lets a client
/// compute which nodes are superseded (`superseded_ids`) and hide them by
/// default; `grafted_from` on an entry lets it render the graft provenance.
pub fn tree_to_json(tree: SessionTree) -> json.Json {
  json.object([
    #("id", json.string(tree.id)),
    #("project", json.string(tree.project)),
    #("active_leaf", json.nullable(tree.active_leaf, json.string)),
    #("allow_domains", json.array(tree.allow_domains, json.string)),
    #("groups", json.array(tree.groups, json.string)),
    #("entries", json.array(list.reverse(tree.entries), entry_to_json)),
    #("grafts", json.array(list.reverse(tree.grafts), graft_event_to_json)),
  ])
}

// --- JSONL persistence ---------------------------------------------------

type Meta {
  Meta(
    id: String,
    project: String,
    active_leaf: Option(String),
    allow_domains: List(String),
    groups: List(String),
  )
}

fn meta_to_json(tree: SessionTree) -> json.Json {
  json.object([
    #("id", json.string(tree.id)),
    #("project", json.string(tree.project)),
    #("active_leaf", json.nullable(tree.active_leaf, json.string)),
    #("allow_domains", json.array(tree.allow_domains, json.string)),
    #("groups", json.array(tree.groups, json.string)),
  ])
}

fn meta_decoder() -> decode.Decoder(Meta) {
  use id <- decode.field("id", decode.string)
  use project <- decode.field("project", decode.string)
  use active_leaf <- decode.field("active_leaf", decode.optional(decode.string))
  // Older session files predate the allowlist — default to empty.
  use allow_domains <- decode.optional_field(
    "allow_domains",
    [],
    decode.list(decode.string),
  )
  // Older session files predate capability groups — default to empty.
  use groups <- decode.optional_field("groups", [], decode.list(decode.string))
  decode.success(Meta(
    id: id,
    project: project,
    active_leaf: active_leaf,
    allow_domains: allow_domains,
    groups: groups,
  ))
}

/// Serialize a tree to JSONL: meta line, entries oldest-first, then graft
/// records oldest-first. Graft lines are tagged with `"op":"graft"` so the
/// parser can tell them from entries.
pub fn to_jsonl(tree: SessionTree) -> String {
  let meta = json.to_string(meta_to_json(tree))
  let entries =
    tree.entries
    |> list.reverse
    |> list.map(fn(e) { json.to_string(entry_to_json(e)) })
  let grafts =
    tree.grafts
    |> list.reverse
    |> list.map(fn(g) { json.to_string(graft_event_to_json(g)) })
  string.join([meta, ..list.append(entries, grafts)], "\n")
}

/// Parse a tree from JSONL produced by `to_jsonl`.
pub fn from_jsonl(contents: String) -> Result(SessionTree, String) {
  let lines =
    contents
    |> string.split("\n")
    |> list.filter(fn(l) { l != "" })

  case lines {
    [] -> Error("empty session file")
    [meta_line, ..record_lines] -> {
      use meta <- result.try(
        json.parse(meta_line, meta_decoder())
        |> result.replace_error("invalid meta line"),
      )
      use #(entries, grafts) <- result.try(parse_records(record_lines, [], []))
      Ok(SessionTree(
        id: meta.id,
        project: meta.project,
        entries: list.reverse(entries),
        active_leaf: meta.active_leaf,
        allow_domains: meta.allow_domains,
        groups: meta.groups,
        grafts: list.reverse(grafts),
      ))
    }
  }
}

/// Split the post-meta lines into entries and graft records (each oldest-first),
/// telling them apart by the `"op":"graft"` tag the graft decoder requires.
fn parse_records(
  lines: List(String),
  entries: List(Entry),
  grafts: List(GraftEvent),
) -> Result(#(List(Entry), List(GraftEvent)), String) {
  case lines {
    [] -> Ok(#(list.reverse(entries), list.reverse(grafts)))
    [l, ..rest] ->
      case json.parse(l, graft_event_decoder()) {
        Ok(g) -> parse_records(rest, entries, [g, ..grafts])
        Error(_) ->
          case json.parse(l, entry_decoder()) {
            Ok(e) -> parse_records(rest, [e, ..entries], grafts)
            Error(_) -> Error("invalid record line")
          }
      }
  }
}
