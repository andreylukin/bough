//// The files in a session's workspace, for the composer's "@" file picker.
//// Tracked + untracked-but-not-ignored paths via `git ls-files` (so the list
//// respects .gitignore the same way the agent's view of the repo does); a
//// bounded `find` fallback when the workspace isn't a git repo.

import gleam/list
import gleam/string
import shellout

// Cap the list so a huge tree can't bloat the response or the client's picker;
// fuzzy matching past a few thousand candidates isn't useful anyway.
const max_files = 20_000

/// Workspace-relative file paths, one per entry, capped at `max_files`.
pub fn list_files(workspace: String) -> List(String) {
  case git_ls(workspace) {
    Ok(files) -> files
    Error(_) -> find_files(workspace)
  }
  |> list.take(max_files)
}

fn git_ls(workspace: String) -> Result(List(String), Nil) {
  case
    shellout.command(
      "git",
      ["-C", workspace, "ls-files", "--cached", "--others", "--exclude-standard"],
      workspace,
      [],
    )
  {
    Ok(out) -> Ok(lines(out))
    Error(_) -> Error(Nil)
  }
}

/// Non-git fallback: every regular file under the workspace, skipping the noisy
/// directories a coding agent never wants to "@". Paths are normalized to drop
/// the leading `./` so they match the git-relative form.
fn find_files(workspace: String) -> List(String) {
  case
    shellout.command(
      "find",
      [
        ".", "-type", "f", "-not", "-path", "*/.git/*", "-not", "-path",
        "*/node_modules/*", "-not", "-path", "*/.build/*", "-not", "-path",
        "*/build/*", "-not", "-path", "*/_build/*", "-not", "-path",
        "*/target/*",
      ],
      workspace,
      [],
    )
  {
    Ok(out) ->
      lines(out)
      |> list.map(fn(p) { string.replace(p, "./", "") })
    Error(_) -> []
  }
}

fn lines(out: String) -> List(String) {
  out
  |> string.split("\n")
  |> list.map(string.trim)
  |> list.filter(fn(l) { l != "" })
}
