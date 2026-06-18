//// Filesystem checkpoints for the session tree (SPEC §4.1): every turn captures
//// the workspace so a `fork` can restore the files to an earlier node, not just
//// the chat — "the filesystem forks with it".
////
//// Backed by a shadow git repo per session under `~/.bough/snapshots/<id>`, with
//// the workspace as its work-tree. This is content-addressed (cheap, deduped
//// across turns) and gives correct add/modify/delete restore via reset+clean —
//// without ever touching the user's own `.git` (separate GIT_DIR, and `.git/` is
//// excluded). A snapshot ref is the commit SHA, stored on the node's entry.
////
//// Disable with `BOUGH_NO_SNAPSHOTS=1` (e.g. very large repos).

import envoy
import gleam/list
import gleam/result
import gleam/string
import shellout
import simplifile

const excludes = ".git/\nnode_modules/\nbuild/\n_build/\ntarget/\n.bough/\n"

/// Capture the workspace as a snapshot, returning its ref (commit SHA). Errors
/// (disabled, unsafe workspace, git failure) are non-fatal to the caller — it
/// just records no snapshot for the turn.
pub fn capture(session_id: String, workspace: String) -> Result(String, String) {
  use _ <- result.try(guard())
  use _ <- result.try(guard_workspace(workspace))
  use gitdir <- result.try(ensure_repo(session_id, workspace))
  use _ <- result.try(git(gitdir, workspace, ["add", "-A"]))
  use _ <- result.try(git(gitdir, workspace, [
    "-c", "user.email=bough@local", "-c", "user.name=bough", "commit",
    "--allow-empty", "--no-gpg-sign", "-q", "-m", "turn",
  ]))
  use sha <- result.try(git(gitdir, workspace, ["rev-parse", "HEAD"]))
  Ok(string.trim(sha))
}

/// Restore the workspace to a snapshot ref: tracked files are reset to that
/// commit and files added since are removed (ignored/excluded paths are left).
pub fn restore(
  session_id: String,
  workspace: String,
  ref: String,
) -> Result(Nil, String) {
  use _ <- result.try(guard())
  use _ <- result.try(guard_workspace(workspace))
  let gitdir = repo_path(session_id)
  use _ <- result.try(case is_repo(gitdir) {
    True -> Ok("")
    False -> Error("no snapshot repo for session " <> session_id)
  })
  use _ <- result.try(git(gitdir, workspace, ["reset", "--hard", "-q", ref]))
  use _ <- result.try(git(gitdir, workspace, ["clean", "-fdq"]))
  Ok(Nil)
}

fn guard() -> Result(Nil, String) {
  case envoy.get("BOUGH_NO_SNAPSHOTS") {
    Ok(_) -> Error("snapshots disabled")
    Error(_) -> Ok(Nil)
  }
}

/// Never run git -C / reset --hard against a dangerous root.
fn guard_workspace(workspace: String) -> Result(Nil, String) {
  let home = envoy.get("HOME") |> result.unwrap("")
  case string.trim(workspace) {
    "" -> Error("empty workspace")
    "/" -> Error("refusing to snapshot /")
    w if w == home -> Error("refusing to snapshot HOME")
    _ -> Ok(Nil)
  }
}

fn repo_path(session_id: String) -> String {
  let home = envoy.get("HOME") |> result.unwrap("/tmp")
  home <> "/.bough/snapshots/" <> session_id
}

fn is_repo(gitdir: String) -> Bool {
  simplifile.is_directory(gitdir <> "/objects") == Ok(True)
}

fn ensure_repo(session_id: String, workspace: String) -> Result(String, String) {
  let gitdir = repo_path(session_id)
  case is_repo(gitdir) {
    True -> Ok(gitdir)
    False -> {
      let _ = simplifile.create_directory_all(gitdir)
      use _ <- result.try(git(gitdir, workspace, ["init", "-q"]))
      let _ = simplifile.write(gitdir <> "/info/exclude", excludes)
      Ok(gitdir)
    }
  }
}

fn git(
  gitdir: String,
  workspace: String,
  args: List(String),
) -> Result(String, String) {
  let full =
    list.flatten([["--git-dir=" <> gitdir, "--work-tree=" <> workspace], args])
  case shellout.command("git", full, workspace, []) {
    Ok(out) -> Ok(out)
    Error(#(_code, message)) -> Error(string.trim(message))
  }
}
