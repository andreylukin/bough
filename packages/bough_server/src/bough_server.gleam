//// bough server entry point.
////
//// Owns all state: the agent loop, the session tree, and nono supervision.
//// Clients (TUI, web, …) are thin and talk to it over HTTP + SSE (SPEC.md §3,
//// §8). HTTP/SSE wiring (wisp/mist) and OTP supervision (gleam_otp) are added
//// once the transport is chosen — see SPEC.md §11.

import gleam/io
import gleam/int

const default_port = 4096

pub fn main() -> Nil {
  serve(default_port)
}

pub fn serve(port: Int) -> Nil {
  io.println("bough server starting on 127.0.0.1:" <> int.to_string(port))
  io.println("not yet implemented — see SPEC.md §8 and §10")
}
