//! No runtime invariant: `tui-render` is a pure library with no catalog row, no service key and no
//! live state (P3-D5). §0.2 asks a plugin crate to check an authoritative event stream or data
//! relation it OWNS over time; this crate owns neither — every claim it makes is a function of its
//! arguments and is checked by `tests/intents.rs`, `tests/wrap.rs` and `tests/args.rs` instead.
