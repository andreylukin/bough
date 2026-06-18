import bough_core/digest
import gleam/string

pub fn short_text_passes_through_test() {
  assert digest.digest("hello", 100) == "hello"
}

pub fn long_text_keeps_head_and_tail_test() {
  let text =
    string.repeat("a", 50) <> string.repeat("b", 50) <> string.repeat("c", 50)
  let d = digest.digest(text, 50)
  assert string.starts_with(d, string.repeat("a", 50))
  assert string.ends_with(d, string.repeat("c", 50))
  assert string.contains(d, "[50 chars elided]")
}

pub fn multibyte_safe_test() {
  let d = digest.digest(string.repeat("é", 300), 100)
  assert string.contains(d, "[100 chars elided]")
  // Head and tail are whole graphemes, never a split multibyte char.
  assert string.starts_with(d, string.repeat("é", 100))
}
