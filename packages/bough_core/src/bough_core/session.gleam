//// The session tree: bough's branchable history.
////
//// Stored as JSONL (one `Entry` per line) under
//// `~/.bough/sessions/<project>/<session-id>.jsonl`. Each entry knows its
//// parent, so the conversation is a tree; the `active_leaf` marks the current
//// position. Branching is appending an entry whose `parent_id` points at an
//// earlier node. See SPEC.md §4.

import gleam/list
import gleam/option.{type Option, None, Some}

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
