//// bough server entry point.
////
//// Owns all state: the agent loop, the session tree, and nono supervision.
//// Clients (TUI, web, …) are thin and talk to it over HTTP + SSE (SPEC.md §3,
//// §8). The slice boots a wisp/mist HTTP server with `/`, `/health`, `/doc`;
//// session supervision and the agent loop land next (SPEC.md §10).

import bough_server/router
import gleam/erlang/process
import gleam/int
import gleam/io
import mist
import wisp
import wisp/wisp_mist

const default_port = 4096

pub fn main() -> Nil {
  serve(default_port)
}

pub fn serve(port: Int) -> Nil {
  wisp.configure_logger()
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
