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

/// The scratch working directory for running a branch in isolation, off the
/// trunk project dir: `~/.bough/work/<session>/<leaf>`.
pub fn worktree_path(session_id: String, leaf_id: String) -> String {
  let home = envoy.get("HOME") |> result.unwrap("/tmp")
  home <> "/.bough/work/" <> session_id <> "/" <> leaf_id
}

/// Materialize a branch's leaf snapshot into its own git worktree of the
/// session's shadow repo, so a run can act on it without touching trunk. Returns
/// the worktree path. Re-materializes a stale dir (worktree is scratch).
pub fn materialize_worktree(
  session_id: String,
  leaf_id: String,
  ref: String,
) -> Result(String, String) {
  use _ <- result.try(guard())
  let gitdir = repo_path(session_id)
  use _ <- result.try(case is_repo(gitdir) {
    True -> Ok("")
    False -> Error("no snapshot repo for session " <> session_id)
  })
  let path = worktree_path(session_id, leaf_id)
  let _ = remove_worktree(session_id, leaf_id)
  let _ = simplifile.create_directory_all(path)
  use _ <- result.try(case
    shellout.command(
      "git",
      ["--git-dir=" <> gitdir, "worktree", "add", "--detach", "-q", path, ref],
      ".",
      [],
    )
  {
    Ok(_) -> Ok("")
    Error(#(_, m)) -> Error(string.trim(m))
  })
  Ok(path)
}

/// Capture a branch worktree's current state as a snapshot, committing through
/// the worktree's own index/HEAD (not the shared shadow HEAD), so it never
/// collides with trunk. Returns the commit SHA.
pub fn capture_worktree(workspace: String) -> Result(String, String) {
  use _ <- result.try(guard())
  use _ <- result.try(guard_workspace(workspace))
  use _ <- result.try(git_c(workspace, ["add", "-A"]))
  use _ <- result.try(git_c(workspace, [
    "-c", "user.email=bough@local", "-c", "user.name=bough", "commit",
    "--allow-empty", "--no-gpg-sign", "-q", "-m", "turn",
  ]))
  use sha <- result.try(git_c(workspace, ["rev-parse", "HEAD"]))
  Ok(string.trim(sha))
}

/// Tear down a branch worktree (best-effort; scratch dir).
pub fn remove_worktree(session_id: String, leaf_id: String) -> Nil {
  let gitdir = repo_path(session_id)
  let path = worktree_path(session_id, leaf_id)
  let _ =
    shellout.command(
      "git",
      ["--git-dir=" <> gitdir, "worktree", "remove", "--force", path],
      ".",
      [],
    )
  let _ = simplifile.delete(path)
  Nil
}

fn git_c(workspace: String, args: List(String)) -> Result(String, String) {
  case shellout.command("git", ["-C", workspace, ..args], workspace, []) {
    Ok(out) -> Ok(out)
    Error(#(_code, message)) -> Error(string.trim(message))
  }
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
