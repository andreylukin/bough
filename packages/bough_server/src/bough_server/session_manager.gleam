//// Owns session persistence. In the OTP design each live session is a
//// supervised process (SPEC.md §3); this module is the JSONL store on disk.
////
//// Sessions are written to `~/.bough/sessions/<id>.jsonl` (see `bough_core`'s
//// `session.to_jsonl`). Per-project subdirectories are a later refinement
//// (SPEC.md §4).

import bough_core/session.{type SessionTree}
import envoy
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
  simplifile.write(path_for(d, tree.id), session.to_jsonl(tree))
  |> result.map_error(fn(e) { Io(string.inspect(e)) })
}

pub fn load(id: String) -> Result(SessionTree, StoreError) {
  use d <- result.try(dir())
  use contents <- result.try(
    simplifile.read(path_for(d, id))
    |> result.map_error(fn(e) { Io(string.inspect(e)) }),
  )
  session.from_jsonl(contents) |> result.map_error(Corrupt)
}
