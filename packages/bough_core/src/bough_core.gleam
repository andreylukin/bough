//// bough_core — shared types and pure logic for the bough coding agent.
////
//// This package has no side effects and no external dependencies beyond
//// gleam_stdlib. The server (`bough_server`) and clients (`bough_tui`) depend
//// on it for a single source of truth over the wire and on disk.
////
//// See ../../SPEC.md for the design.

pub const version = "0.1.0"
