//// Owns session persistence. In the OTP design each live session is a
//// supervised process (SPEC.md §3); this module is the JSONL store on disk.
////
//// Sessions are written to `~/.bough/sessions/<id>.jsonl` (see `bough_core`'s
//// `session.to_jsonl`). Per-project subdirectories are a later refinement
//// (SPEC.md §4).

import bough_core/session.{type SessionTree, SessionTree}
import envoy
import gleam/int
import gleam/option.{None, Some}
import gleam/list
import gleam/result
import gleam/string
import simplifile

pub type StoreError {
  NoHome
  Io(String)
  Corrupt(String)
}

/// Ensure and return the sessions directory.
pub fn dir() -> Result(String, StoreError) {
  use home <- result.try(envoy.get("HOME") |> result.replace_error(NoHome))
  let d = home <> "/.bough/sessions"
  use _ <- result.try(
    simplifile.create_directory_all(d)
    |> result.map_error(fn(e) { Io(string.inspect(e)) }),
  )
  Ok(d)
}

fn path_for(d: String, id: String) -> String {
  d <> "/" <> id <> ".jsonl"
}

pub fn save(tree: SessionTree) -> Result(Nil, StoreError) {
  use d <- result.try(dir())
  // Write-temp-then-rename so a concurrent reader (another branch's run) never
  // sees a half-written file: rename is atomic, so a load gets either the old
  // or the new complete tree, never a truncated one.
  let target = path_for(d, tree.id)
  let tmp = target <> ".tmp"
  use _ <- result.try(
    simplifile.write(tmp, session.to_jsonl(tree))
    |> result.map_error(fn(e) { Io(string.inspect(e)) }),
  )
  simplifile.rename(tmp, target)
  |> result.map_error(fn(e) { Io(string.inspect(e)) })
}

pub fn load(id: String) -> Result(SessionTree, StoreError) {
  use d <- result.try(dir())
  use contents <- result.try(
    simplifile.read(path_for(d, id))
    |> result.map_error(fn(e) { Io(string.inspect(e)) }),
  )
  use tree <- result.map(
    session.from_jsonl(contents) |> result.map_error(Corrupt),
  )
  // Legacy files predate the trunk pointer; before it, the working dir always
  // tracked the active branch, so seed trunk = active_leaf on first load.
  let trunk = case tree.trunk_leaf {
    Some(_) -> tree.trunk_leaf
    None -> tree.active_leaf
  }
  SessionTree(..tree, project: expand_home(tree.project), trunk_leaf: trunk)
}

/// The project path is the sandbox workspace, handed to the monty/nono sidecar
/// as an argv/cwd string — which, unlike a shell, never expands a leading `~`
/// or `$HOME`. Normalize on load so every consumer (run, diff, snapshots) sees
/// an absolute path the sandbox can actually find, whatever the user typed.
fn expand_home(path: String) -> String {
  case envoy.get("HOME") {
    Ok(home) if home != "" ->
      case path {
        "~" | "$HOME" -> home
        "~/" <> rest | "$HOME/" <> rest -> home <> "/" <> rest
        _ -> path
      }
    _ -> path
  }
}

/// A one-line summary of a stored session, for the resume picker.
pub type Summary {
  Summary(
    id: String,
    project: String,
    title: String,
    turns: Int,
    updated: Int,
  )
}

/// All stored sessions, most-recently-updated first.
pub fn list() -> Result(List(Summary), StoreError) {
  use d <- result.try(dir())
  use names <- result.try(
    simplifile.read_directory(d)
    |> result.map_error(fn(e) { Io(string.inspect(e)) }),
  )
  names
  |> list.filter(string.ends_with(_, ".jsonl"))
  |> list.filter_map(fn(name) {
    let id = string.drop_end(name, 6)
    case load(id) {
      Ok(tree) -> Ok(summarize(tree))
      Error(_) -> Error(Nil)
    }
  })
  // Drop never-used sessions so the picker stays clean.
  |> list.filter(fn(s) { s.turns > 0 })
  |> list.sort(fn(a, b) { int.compare(b.updated, a.updated) })
  |> Ok
}

fn summarize(tree: SessionTree) -> Summary {
  let title =
    tree.entries
    |> list.reverse
    |> list.find(fn(e) { e.role == session.User })
    |> result.map(fn(e) { e.content })
    |> result.unwrap("(empty)")
  let updated =
    tree.entries
    |> list.fold(0, fn(acc, e) { int.max(acc, e.timestamp) })
  Summary(
    id: tree.id,
    project: tree.project,
    title: title,
    turns: list.length(tree.entries),
    updated: updated,
  )
}
