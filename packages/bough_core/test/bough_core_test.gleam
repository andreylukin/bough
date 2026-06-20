import bough_core/session.{Assistant, Entry, User}
import gleam/list
import gleam/option.{None, Some}
import gleam/string
import gleeunit

pub fn main() -> Nil {
  gleeunit.main()
}

pub fn jsonl_round_trip_test() {
  let tree =
    session.new("s1", "/proj")
    |> session.append(Entry("e1", None, User, "hi", None, None, 1, None))
    |> session.append(Entry("e2", Some("e1"), Assistant, "yo", None, None, 2, None))

  let assert Ok(parsed) = session.from_jsonl(session.to_jsonl(tree))
  assert parsed == tree
}

pub fn append_sets_active_leaf_test() {
  let tree =
    session.new("s1", "/proj")
    |> session.append(Entry("e1", None, User, "hi", None, None, 1, None))

  assert tree.active_leaf == Some("e1")
  assert session.children_of(tree, None)
    == [Entry("e1", None, User, "hi", None, None, 1, None)]
}

/// A linear branch a→b grafted onto a separate node c: the copies hang off an
/// injected marker under c, are stamped with `grafted_from`, carry no snapshot,
/// the originals are untouched but superseded, and the leaf lands on the copy
/// of the section tip. Generated ids are `<salt>-<original>`.
pub fn graft_reparents_section_test() {
  let tree =
    session.new("s1", "/proj")
    |> session.append(Entry("a", None, User, "task", Some("snapA"), None, 1, None))
    |> session.append(Entry("b", Some("a"), Assistant, "did it", Some("snapB"), None, 2, None))
    |> session.append(Entry("c", None, User, "other base", Some("snapC"), None, 3, None))

  let assert Ok(graft) = session.plan_graft(tree, "a", "c", "g1", 9)
  let grafted = session.apply_graft(tree, graft)

  // Marker bridges c → copies; copies re-parent within the section.
  let assert Some(marker) = session.get(grafted, "g1-marker")
  assert marker.parent_id == Some("c")
  assert marker.content == session.graft_marker

  let assert Some(a2) = session.get(grafted, "g1-a")
  assert a2.parent_id == Some("g1-marker")
  assert a2.grafted_from == Some("a")
  assert a2.snapshot_ref == None
  assert a2.content == "task"

  let assert Some(b2) = session.get(grafted, "g1-b")
  assert b2.parent_id == Some("g1-a")
  assert b2.grafted_from == Some("b")

  // Originals untouched; now superseded. Leaf landed on the tip's copy.
  let assert Some(a) = session.get(grafted, "a")
  assert a.parent_id == None
  assert list.sort(session.superseded_ids(grafted), string.compare) == ["a", "b"]
  assert grafted.active_leaf == Some("g1-b")

  // The whole thing round-trips through JSONL (graft record included).
  let assert Ok(reparsed) = session.from_jsonl(session.to_jsonl(grafted))
  assert reparsed == grafted
}

pub fn graft_rejects_cycle_test() {
  let tree =
    session.new("s1", "/proj")
    |> session.append(Entry("a", None, User, "root", None, None, 1, None))
    |> session.append(Entry("b", Some("a"), Assistant, "child", None, None, 2, None))

  // Grafting a onto its own descendant b would cycle.
  assert session.plan_graft(tree, "a", "b", "g1", 9) == Error(session.GraftCycle)

  assert session.plan_graft(tree, "nope", "a", "g1", 9)
    == Error(session.GraftNodeNotFound("nope"))
}
