//// Wall-clock time. Kept in the server (not core) so `bough_core` stays pure.

@external(erlang, "bough_ffi", "now_ms")
pub fn now_ms() -> Int
