//// bough server entry point.
////
//// Owns all state: the agent loop and the session tree. Clients (TUI, web, …)
//// are thin and talk to it over HTTP + SSE (SPEC.md §3, §8). Boots a wisp/mist
//// HTTP server with the JSON API and the web client.

import bough_server/proxy
import bough_server/router
import envoy
import gleam/erlang/process
import gleam/int
import gleam/io
import gleam/result
import mist
import wisp
import wisp/wisp_mist

const default_port = 4096

pub fn main() -> Nil {
  let port =
    envoy.get("BOUGH_PORT")
    |> result.try(int.parse)
    |> result.unwrap(default_port)
  serve(port)
}

pub fn serve(port: Int) -> Nil {
  wisp.configure_logger()
  // Sweep any per-session mitmproxies left running by a previous bough process.
  proxy.cleanup_all()
  let secret_key_base = wisp.random_string(64)

  let assert Ok(_) =
    wisp_mist.handler(router.handle_request, secret_key_base)
    |> mist.new
    |> mist.port(port)
    |> mist.bind("127.0.0.1")
    |> mist.start

  io.println(
    "bough server listening on http://127.0.0.1:" <> int.to_string(port),
  )
  process.sleep_forever()
}
