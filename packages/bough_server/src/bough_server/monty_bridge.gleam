//// Bridge to the `bough-monty` code-mode sidecar (SPEC.md §5.2). The supervisor
//// emits a Python program per round; this runs it inside a monty sandbox where
//// the only doors out are the host functions `bash`/`read`/`write`/`edit`
//// (`bash` shells through nono). The sidecar is a Rust binary embedding the
//// monty interpreter — the BEAM can't host monty in-process, so bough drives it
//// the same way it drives nono: one execve per round via `shellout`.
////
//// The program travels as a single argv element (`--code-str`), so there is no
//// shell and nothing to escape. The sidecar always exits 0 and reports
//// success/failure in its JSON, so a Python error is never confused with a
//// process failure.

import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/string
import shellout

/// Run one Python program in the monty sandbox over `workspace`. Returns an
/// exit-code-style pair (`0` on success, `1` on a Python/sidecar error) plus the
/// captured stdout (or the error text), matching the engine's `Exec` shape.
pub fn run_code(
  workspace: String,
  code: String,
  profile: Option(String),
) -> #(Int, String) {
  let profile_args = case profile {
    Some(path) -> ["--nono-profile", path]
    None -> []
  }
  case
    shellout.command(
      binary_path(),
      list.append(["--workspace", workspace, "--code-str", code], profile_args),
      workspace,
      [],
    )
  {
    Ok(out) -> parse_result(out)
    Error(#(_code, out)) ->
      #(1, "monty sidecar failed to run (is bough-monty installed?): " <> out)
  }
}

/// The sidecar binary: `BOUGH_MONTY_BIN`, else the installed `~/.bough/bin`
/// copy, else bare `bough-monty` on PATH.
fn binary_path() -> String {
  case envoy.get("BOUGH_MONTY_BIN") {
    Ok(path) -> path
    Error(_) ->
      case envoy.get("HOME") {
        Ok(home) -> home <> "/.bough/bin/bough-monty"
        Error(_) -> "bough-monty"
      }
  }
}

fn parse_result(raw: String) -> #(Int, String) {
  let decoder = {
    use ok <- decode.field("ok", decode.bool)
    use output <- decode.field("output", decode.string)
    use error <- decode.field("error", decode.string)
    decode.success(#(ok, output, error))
  }
  case json.parse(string.trim(raw), decoder) {
    Ok(#(True, output, _)) -> #(0, output)
    Ok(#(False, output, error)) -> #(1, join(output, error))
    Error(_) -> #(1, "could not parse monty result: " <> raw)
  }
}

/// Combine captured stdout with the error message — the model wants both the
/// output it printed before failing and the failure itself.
fn join(output: String, error: String) -> String {
  case string.trim(output) {
    "" -> error
    _ -> output <> "\n" <> error
  }
}
