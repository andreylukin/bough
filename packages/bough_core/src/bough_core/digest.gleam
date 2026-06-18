//// The blackboard digest (SPEC.md §5.3): full step output is saved to a file;
//// the conversation carries only a short head+tail excerpt plus a pointer, so
//// context stays small and prefix-cacheable. Pure; ported from tent's
//// `engine/blackboard.rs::digest`.

import gleam/int
import gleam/string

/// A deterministic head+tail digest of `text`, clipped to `limit` graphemes at
/// each end. Short text passes through unchanged. Grapheme-based, so multibyte
/// content is never split mid-character.
pub fn digest(text: String, limit: Int) -> String {
  let text = string.trim(text)
  let n = string.length(text)
  case n <= 2 * limit {
    True -> text
    False -> {
      let head = string.slice(text, 0, limit)
      let tail = string.slice(text, n - limit, limit)
      let elided = n - 2 * limit
      head
      <> "\n... ["
      <> int.to_string(elided)
      <> " chars elided] ...\n"
      <> tail
    }
  }
}
