//// The agent's uncommitted work product in a session's workspace, for review:
//// a unified diff (tracked changes vs HEAD, plus untracked files as new-file
//// diffs) and a per-file status list. Reads the workspace's *own* git — never
//// the shadow snapshot repo — so "changes" means "what isn't committed yet",
//// which is what a human reviews before keeping the agent's work.

import gleam/list
import gleam/string
import shellout

pub type FileChange {
  FileChange(status: String, path: String)
}

// Cap the patch so a giant generated file can't bloat the response; the file
// list still names everything that changed.
const max_patch = 200_000

/// `#(is_git, files, patch)` for `workspace` — empty when clean or not a repo.
pub fn working_diff(workspace: String) -> #(Bool, List(FileChange), String) {
  case git(workspace, ["rev-parse", "--is-inside-work-tree"]) {
    Error(_) -> #(False, [], "")
    Ok(_) -> {
      let files = status(workspace)
      // No HEAD yet (a fresh repo) makes `diff HEAD` fail; untracked diffs still
      // render, so the review isn't empty.
      let tracked = case git(workspace, ["diff", "HEAD"]) {
        Ok(d) -> d
        Error(_) -> ""
      }
      let untracked =
        files
        |> list.filter(fn(f) { f.status == "?" })
        |> list.map(fn(f) { untracked_diff(workspace, f.path) })
        |> string.concat
      #(True, files, clip(tracked <> untracked))
    }
  }
}

fn status(workspace: String) -> List(FileChange) {
  case git(workspace, ["status", "--porcelain"]) {
    Error(_) -> []
    Ok(out) ->
      out
      |> string.split("\n")
      |> list.filter_map(parse_status_line)
  }
}

/// Porcelain v1 line: `XY<space>path`. We keep the first status letter (or `?`
/// for untracked) and the path (renames show as `old -> new`, fine for review).
fn parse_status_line(line: String) -> Result(FileChange, Nil) {
  case string.length(line) >= 4 {
    False -> Error(Nil)
    True -> {
      let code = string.trim(string.slice(line, 0, 2))
      let path = string.trim(string.drop_start(line, 3))
      let status = case code {
        "??" | "" -> "?"
        _ -> string.slice(code, 0, 1)
      }
      Ok(FileChange(status: status, path: path))
    }
  }
}

/// A new (untracked) file rendered as a unified "new file" diff. `--no-index`
/// never touches the repo; it exits 1 because the sides differ, so take the
/// captured patch from either arm.
fn untracked_diff(workspace: String, path: String) -> String {
  case
    shellout.command(
      "git",
      ["-C", workspace, "diff", "--no-index", "--", "/dev/null", path],
      workspace,
      [],
    )
  {
    Ok(out) -> out
    Error(#(_, out)) -> out
  }
}

fn git(workspace: String, args: List(String)) -> Result(String, Nil) {
  case shellout.command("git", ["-C", workspace, ..args], workspace, []) {
    Ok(out) -> Ok(out)
    Error(_) -> Error(Nil)
  }
}

fn clip(patch: String) -> String {
  case string.length(patch) > max_patch {
    True -> string.slice(patch, 0, max_patch) <> "\n… (diff truncated)"
    False -> patch
  }
}
