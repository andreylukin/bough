//// Worker runtime (SPEC.md §5.6): "the worker runs as part of bough" means
//// bough owns and supervises a small inference server as a child process — not
//// that you install a separate Ollama daemon. The BEAM can't run inference, so
//// this launches a bundled `llama-server` (the engine Ollama itself wraps),
//// downloads the GGUF on first use, and hands the engine a localhost
//// OpenAI-compatible endpoint.
////
//// Escape hatches: set `BOUGH_WORKER_URL` to point at any remote/own endpoint
//// (this returns it untouched), or leave the worker disabled in the engine
//// config (the supervisor then does its own fixes).
////
//// Deferred: graceful shutdown / supervised restart. The child is spawned
//// unlinked and lives for the BEAM's lifetime (SPEC.md §11).

import envoy
import gleam/erlang/process
import gleam/http.{Get}
import gleam/http/request
import gleam/http/response
import gleam/httpc
import gleam/int
import gleam/result
import shellout
import simplifile

const default_gguf = "qwen2.5-coder-7b-instruct-q4_k_m.gguf"

/// Ensure a worker endpoint is reachable and return its base URL. Reuses an
/// already-running server (or `BOUGH_WORKER_URL`); otherwise downloads the
/// model if needed and starts `llama-server`, waiting for it to become healthy.
pub fn ensure(port: Int) -> Result(String, String) {
  case envoy.get("BOUGH_WORKER_URL") {
    Ok(url) -> Ok(url)
    Error(_) -> {
      let url = "http://127.0.0.1:" <> int.to_string(port)
      case healthy(url) {
        True -> Ok(url)
        False -> {
          use model <- result.try(ensure_model())
          start_server(model, port)
          case wait_healthy(url, 90) {
            True -> Ok(url)
            False ->
              Error("worker server did not become healthy at " <> url)
          }
        }
      }
    }
  }
}

fn ensure_model() -> Result(String, String) {
  use home <- result.try(
    envoy.get("HOME") |> result.replace_error("HOME is not set"),
  )
  let dir = home <> "/.bough/models"
  let _ = simplifile.create_directory_all(dir)
  let filename = envoy.get("BOUGH_WORKER_GGUF") |> result.unwrap(default_gguf)
  let path = dir <> "/" <> filename

  case simplifile.is_file(path) {
    Ok(True) -> Ok(path)
    _ ->
      case envoy.get("BOUGH_WORKER_GGUF_URL") {
        Error(_) ->
          Error(
            "worker model missing at "
            <> path
            <> " and BOUGH_WORKER_GGUF_URL is not set (point it at a GGUF to download, or set BOUGH_WORKER_URL to a running endpoint)",
          )
        Ok(gguf_url) ->
          case shellout.command("curl", ["-fSL", "-o", path, gguf_url], ".", []) {
            Ok(_) -> Ok(path)
            Error(#(_code, message)) ->
              Error("worker model download failed: " <> message)
          }
      }
  }
}

fn start_server(model_path: String, port: Int) -> Nil {
  let bin = envoy.get("BOUGH_LLAMA_SERVER") |> result.unwrap("llama-server")
  // Unlinked: a crash in inference must not take down the agent (SPEC.md §5.6).
  let _ =
    process.spawn_unlinked(fn() {
      let _ =
        shellout.command(
          bin,
          [
            "-m", model_path, "--host", "127.0.0.1", "--port",
            int.to_string(port),
          ],
          ".",
          [],
        )
      Nil
    })
  Nil
}

fn healthy(url: String) -> Bool {
  case request.to(url <> "/health") {
    Error(_) -> False
    Ok(base) ->
      case httpc.send(request.set_method(base, Get)) {
        Ok(response.Response(status: 200, ..)) -> True
        _ -> False
      }
  }
}

fn wait_healthy(url: String, retries: Int) -> Bool {
  case retries <= 0 {
    True -> False
    False ->
      case healthy(url) {
        True -> True
        False -> {
          process.sleep(1000)
          wait_healthy(url, retries - 1)
        }
      }
  }
}
