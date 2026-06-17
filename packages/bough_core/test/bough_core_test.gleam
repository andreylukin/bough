import bough_core/session.{Assistant, Entry, User}
import gleam/option.{None, Some}
import gleeunit

pub fn main() -> Nil {
  gleeunit.main()
}

pub fn jsonl_round_trip_test() {
  let tree =
    session.new("s1", "/proj")
    |> session.append(Entry("e1", None, User, "hi", None, None, 1))
    |> session.append(Entry("e2", Some("e1"), Assistant, "yo", None, None, 2))

  let assert Ok(parsed) = session.from_jsonl(session.to_jsonl(tree))
  assert parsed == tree
}

pub fn append_sets_active_leaf_test() {
  let tree =
    session.new("s1", "/proj")
    |> session.append(Entry("e1", None, User, "hi", None, None, 1))

  assert tree.active_leaf == Some("e1")
  assert session.children_of(tree, None) == [Entry("e1", None, User, "hi", None, None, 1)]
}
