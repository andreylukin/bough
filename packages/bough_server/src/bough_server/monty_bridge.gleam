//// Bridge to the `bough-monty` code-mode sidecar (SPEC.md §5.2). The supervisor
//// emits a Python program per round; this runs it inside a monty sandbox where
//// the only doors out are the host functions `bash`/`read`/`write`/`edit`
//// (`bash` shells through the seatbelt sandbox). The sidecar is a Rust binary
//// embedding the monty interpreter — the BEAM can't host monty in-process, so
//// bough drives it the same way it drives the sandbox: one execve per round via
//// `shellout`.
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
import gleam/result
import gleam/string
import shellout

/// Run one Python program in the monty sandbox over `workspace`. Returns an
/// exit-code-style pair (`0` on success, `1` on a Python/sidecar error) plus the
/// captured stdout (or the error text), matching the engine's `Exec` shape.
///
/// The sidecar runs unsandboxed (trusted) with `read`/`write`/`edit` path-scoped
/// in-process; its `bash` host function wraps each command in a macOS Seatbelt
/// profile (`--seatbelt-profile`) for the filesystem sandbox.
pub fn run_code(
  workspace: String,
  code: String,
  profile: Option(String),
  env: List(#(String, String)),
) -> #(Int, String) {
  let profile_args = case profile {
    Some(path) -> ["--seatbelt-profile", path]
    None -> []
  }
  // Pass the proxy env per-invocation (not via bough's global env) so concurrent
  // sessions on different proxy ports can't clobber each other.
  let opts = case env {
    [] -> []
    _ -> [shellout.SetEnvironment(env)]
  }
  case
    shellout.command(
      binary_path(),
      list.append(["--workspace", workspace, "--code-str", code], profile_args),
      workspace,
      opts,
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
  case json.parse(result_line(raw), decoder) {
    Ok(#(True, output, _)) -> #(0, output)
    Ok(#(False, output, error)) -> #(1, join(output, error))
    Error(_) -> #(1, "could not parse monty result: " <> raw)
  }
}

/// The sidecar emits its result as a single JSON line. When wrapped in the
/// sandbox the output can be flanked by banner/audit chatter, so pick the lone line
/// that is a JSON object rather than trusting the whole capture.
fn result_line(raw: String) -> String {
  raw
  |> string.split("\n")
  |> list.map(string.trim)
  |> list.filter(fn(l) {
    string.starts_with(l, "{") && string.ends_with(l, "}")
  })
  |> list.last
  |> result.unwrap(string.trim(raw))
}

/// Combine captured stdout with the error message — the model wants both the
/// output it printed before failing and the failure itself.
fn join(output: String, error: String) -> String {
  case string.trim(output) {
    "" -> error
    _ -> output <> "\n" <> error
  }
}
