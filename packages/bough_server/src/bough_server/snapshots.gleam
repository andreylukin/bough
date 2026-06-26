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

/// True if `project` is a git working tree with a committed HEAD — the
/// precondition for backing a branch run with a worktree of the real repo.
pub fn is_git_repo(project: String) -> Bool {
  case
    shellout.command(
      "git",
      ["-C", project, "rev-parse", "--verify", "-q", "HEAD"],
      project,
      [],
    )
  {
    Ok(_) -> True
    Error(_) -> False
  }
}

/// Materialize a branch run on a worktree of the user's REAL repo — so its git
/// history, branches and `origin` are all present and `git commit`/`push`/`pull`
/// work natively — then overlay the branch's snapshot files on top so the
/// working tree reflects the branch, not trunk. The real `.git` object store is
/// shared, but capture goes to the SHADOW repo (`capture_real_worktree`), so the
/// user's own history is never written. Returns the worktree path.
pub fn materialize_real_worktree(
  project: String,
  session_id: String,
  run_key: String,
  ref: String,
) -> Result(String, String) {
  use _ <- result.try(guard())
  let shadow = repo_path(session_id)
  use _ <- result.try(case is_repo(shadow) {
    True -> Ok("")
    False -> Error("no snapshot repo for session " <> session_id)
  })
  let path = worktree_path(session_id, run_key)
  remove_real_worktree(project, session_id, run_key)
  let _ = simplifile.create_directory_all(path)
  // A detached worktree of the real repo at its current HEAD: real .git, real
  // remotes, real history.
  use _ <- result.try(real_git(project, [
    "worktree", "add", "--detach", "-q", path, "HEAD",
  ]))
  // Overlay the branch's snapshot onto it via a PRIVATE index, so neither the
  // shadow's shared HEAD/index nor the real repo's index is disturbed:
  //   read-tree   → private index = snapshot
  //   checkout-index -a -f → write snapshot files into the worktree (over HEAD's)
  //   clean -fdq  → drop HEAD files absent from the snapshot (excludes protect .git)
  let idx = path <> ".idx"
  let _ = simplifile.delete(idx)
  use _ <- result.try(idx_git(shadow, path, idx, ["read-tree", ref]))
  use _ <- result.try(idx_git(shadow, path, idx, ["checkout-index", "-a", "-f"]))
  use _ <- result.try(idx_git(shadow, path, idx, ["clean", "-fdq", "-e", ".git"]))
  let _ = simplifile.delete(idx)
  Ok(path)
}

/// Snapshot a real-repo worktree's current files into the session's SHADOW repo
/// (never the user's history): a private index + `commit-tree` parented on the
/// branch's prior snapshot, so no shared HEAD moves. Returns the commit SHA.
pub fn capture_real_worktree(
  session_id: String,
  workspace: String,
  parent_ref: String,
) -> Result(String, String) {
  use _ <- result.try(guard())
  use _ <- result.try(guard_workspace(workspace))
  let shadow = repo_path(session_id)
  let idx = workspace <> ".cap.idx"
  let _ = simplifile.delete(idx)
  use _ <- result.try(idx_git(shadow, workspace, idx, ["add", "-A"]))
  use tree <- result.try(idx_git(shadow, workspace, idx, ["write-tree"]))
  let env = [
    #("GIT_INDEX_FILE", idx),
    #("GIT_AUTHOR_NAME", "bough"), #("GIT_AUTHOR_EMAIL", "bough@local"),
    #("GIT_COMMITTER_NAME", "bough"), #("GIT_COMMITTER_EMAIL", "bough@local"),
  ]
  use commit <- result.try(case
    shellout.command(
      "git",
      [
        "--git-dir=" <> shadow, "commit-tree", string.trim(tree), "-p",
        parent_ref, "-m", "turn",
      ],
      workspace,
      [shellout.SetEnvironment(env)],
    )
  {
    Ok(out) -> Ok(string.trim(out))
    Error(#(_, m)) -> Error(string.trim(m))
  })
  let _ = simplifile.delete(idx)
  Ok(commit)
}

/// Tear down a real-repo worktree (best-effort; unregisters it from the real
/// repo, prunes the registration, and removes the scratch dir + temp indexes).
pub fn remove_real_worktree(
  project: String,
  session_id: String,
  run_key: String,
) -> Nil {
  let path = worktree_path(session_id, run_key)
  let _ = real_git(project, ["worktree", "remove", "--force", path])
  let _ = real_git(project, ["worktree", "prune"])
  let _ = simplifile.delete(path)
  let _ = simplifile.delete(path <> ".idx")
  let _ = simplifile.delete(path <> ".cap.idx")
  Nil
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

/// Run git against the user's real repo (`-C project`).
fn real_git(project: String, args: List(String)) -> Result(String, String) {
  case shellout.command("git", ["-C", project, ..args], project, []) {
    Ok(out) -> Ok(out)
    Error(#(_code, message)) -> Error(string.trim(message))
  }
}

/// Run git against `gitdir` with `workspace` as the work-tree and a PRIVATE
/// index file (`GIT_INDEX_FILE=idx`), so staging/checkout touch neither the
/// gitdir's shared index nor any other repo's.
fn idx_git(
  gitdir: String,
  workspace: String,
  idx: String,
  args: List(String),
) -> Result(String, String) {
  let full =
    list.flatten([["--git-dir=" <> gitdir, "--work-tree=" <> workspace], args])
  case
    shellout.command("git", full, workspace, [
      shellout.SetEnvironment([#("GIT_INDEX_FILE", idx)]),
    ])
  {
    Ok(out) -> Ok(out)
    Error(#(_code, message)) -> Error(string.trim(message))
  }
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
