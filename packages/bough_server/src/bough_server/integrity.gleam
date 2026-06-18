//// Integrity tracking (SPEC.md §5.4): hash every pre-existing workspace file at
//// task start so the review step can tell the supervisor which ones it touched.
//// An earned guardrail — a supervisor was caught rewriting a task's reference
//// file to make its own check pass. Ported from tent's `engine/guardrails.rs`.

import gleam/dict.{type Dict}
import gleam/list
import gleam/string
import simplifile

@external(erlang, "bough_ffi", "hash")
fn hash(data: String) -> Int

const skip_dirs = ["/.git/", "/node_modules/", "/target/", "/.bough/"]

/// Content hash of every regular file under `root` (relative path -> hash),
/// skipping VCS/build directories. Best-effort: unreadable files are omitted.
pub fn snapshot(root: String) -> Dict(String, Int) {
  case simplifile.get_files(root) {
    Error(_) -> dict.new()
    Ok(files) ->
      files
      |> list.filter(fn(p) { !is_skipped(p) })
      |> list.fold(dict.new(), fn(acc, p) {
        case simplifile.read(p) {
          Ok(content) -> dict.insert(acc, relative(root, p), hash(content))
          Error(_) -> acc
        }
      })
  }
}

/// Pre-existing files modified or deleted since `baseline`, sorted. New files
/// are not reported — only the human's existing work being altered.
pub fn changed_preexisting(
  root: String,
  baseline: Dict(String, Int),
) -> List(String) {
  let current = snapshot(root)
  baseline
  |> dict.to_list
  |> list.filter(fn(kv) { dict.get(current, kv.0) != Ok(kv.1) })
  |> list.map(fn(kv) { kv.0 })
  |> list.sort(string.compare)
}

fn is_skipped(path: String) -> Bool {
  list.any(skip_dirs, fn(s) { string.contains(path, s) })
}

fn relative(root: String, path: String) -> String {
  case string.starts_with(path, root <> "/") {
    True -> string.drop_start(path, string.length(root) + 1)
    False -> path
  }
}
