//! Invariant: the reconciler never calls a lifecycle method. It diffs two trees by row id and
//! writes each fiber's `target`; the drivers converge. That is what makes the quiescent state a
//! function of the FINAL tree alone, independent of the order the diff was walked (§0.3, §0.5).

use crate::config::{Composition, Entry, Expr};
use crate::fiber::EntryId;

/// The only thing a diff produces.
#[derive(Clone, Debug, PartialEq)]
pub enum TargetWrite {
    /// A row present only in the new tree.
    Create { id: EntryId },
    /// `plugin` changed: dispose the old fiber entirely and create a new one, with a new
    /// [`crate::fiber::FiberUid`].
    ///
    /// DEVIATION from plan §2.8: the variant does not carry the previous `FiberUid`. The diff is
    /// pure — it sees two trees and no fiber table — and the applier already knows which fiber
    /// holds the row. Carrying it here would have forced the fiber table into a pure function.
    Rebuild { id: EntryId },
    /// Hand the new config to the plugin via `reconfigure`; `Applied` ⇒ nothing, `Reload` ⇒
    /// unload then load.
    Reconfigure { id: EntryId },
    /// `disabled` false→true, or the row is absent in the new tree.
    Unload { id: EntryId },
    /// `disabled` true→false; PENDING until the row's keys resolve.
    Load { id: EntryId },
    /// `isolate` changed: the committed view would be resolved in a different realm, so unload
    /// then load unconditionally.
    Reload { id: EntryId },
    /// `inject` changed: recompute the dependency targets and reload IFF a resolved
    /// `ProviderUid` differs (§0.3).
    ///
    /// DEVIATION from plan §2.8, which folds this into `Reload`: the two are different orders —
    /// one always reloads, one only reloads when the resolution actually moved — and the plan's
    /// own test `inject_change_reloads_only_when_a_target_differs` needs them distinguishable.
    Retarget { id: EntryId },
}

impl TargetWrite {
    pub fn id(&self) -> &EntryId {
        match self {
            TargetWrite::Create { id }
            | TargetWrite::Rebuild { id }
            | TargetWrite::Reconfigure { id }
            | TargetWrite::Unload { id }
            | TargetWrite::Load { id }
            | TargetWrite::Reload { id }
            | TargetWrite::Retarget { id } => id,
        }
    }
}

/// A row's `disabled`, as resolved on an evaluated tree. An unevaluated `!!expr` reaching here is
/// a composition bug, not a runtime decision: treat it as enabled so boot fails loud on the row
/// rather than silently skipping it.
pub fn is_disabled(e: &Entry) -> bool {
    match &e.disabled {
        Expr::Literal(b) => *b,
        Expr::Source(_) => false,
    }
}

/// Per-field reconciliation, exactly as tabulated in §0.3 / plan §2.8. Pure: it reads two trees
/// and produces target writes.
pub fn diff(old: &Composition, new: &Composition) -> Vec<TargetWrite> {
    diff_trees(&old.tree, &new.tree)
}

/// The pure core, over the two evaluated trees.
pub fn diff_trees(old: &[Entry], new: &[Entry]) -> Vec<TargetWrite> {
    let mut out = Vec::new();
    let old_rows = flatten(old);
    let new_rows = flatten(new);
    // New tree order first, so the writes read like the tree does.
    for (id, n) in &new_rows {
        let o = old_rows.iter().find(|(oid, _)| oid == id).map(|(_, e)| *e);
        diff_row(o, Some(n), &mut out);
    }
    for (id, o) in &old_rows {
        if !new_rows.iter().any(|(nid, _)| nid == id) {
            diff_row(Some(o), None, &mut out);
        }
    }
    out
}

/// Per-row diff. Group children are diffed as rows in their own right (they are fibers, and
/// effects of the parent); this function does not recurse, [`diff_trees`] flattens for it.
pub fn diff_row(old: Option<&Entry>, new: Option<&Entry>, out: &mut Vec<TargetWrite>) {
    match (old, new) {
        (None, None) => {}
        (None, Some(n)) => {
            if !is_disabled(n) {
                out.push(TargetWrite::Create { id: n.id.clone() });
            } else {
                // A row that arrives already disabled still gets a fiber, resting INACTIVE, so a
                // later `disabled: false` is a Load and not a Create.
                out.push(TargetWrite::Create { id: n.id.clone() });
                out.push(TargetWrite::Unload { id: n.id.clone() });
            }
        }
        (Some(o), None) => out.push(TargetWrite::Unload { id: o.id.clone() }),
        (Some(o), Some(n)) => {
            if o.plugin != n.plugin {
                out.push(TargetWrite::Rebuild { id: n.id.clone() });
                return;
            }
            match (is_disabled(o), is_disabled(n)) {
                (false, true) => {
                    out.push(TargetWrite::Unload { id: n.id.clone() });
                    return;
                }
                (true, false) => out.push(TargetWrite::Load { id: n.id.clone() }),
                _ => {}
            }
            if o.isolate != n.isolate {
                out.push(TargetWrite::Reload { id: n.id.clone() });
            }
            if o.inject != n.inject {
                out.push(TargetWrite::Retarget { id: n.id.clone() });
            }
            if o.config != n.config {
                out.push(TargetWrite::Reconfigure { id: n.id.clone() });
            }
        }
    }
}

/// Depth-first flatten: a row, then its group children. Children are rows in their own right.
pub fn flatten(tree: &[Entry]) -> Vec<(EntryId, &Entry)> {
    fn walk<'a>(rows: &'a [Entry], out: &mut Vec<(EntryId, &'a Entry)>) {
        for e in rows {
            out.push((e.id.clone(), e));
            walk(&e.group, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// Every row's parent, for mounting group children as effects of their parent.
pub fn parents(tree: &[Entry]) -> Vec<(EntryId, Option<EntryId>)> {
    fn walk(rows: &[Entry], parent: Option<&EntryId>, out: &mut Vec<(EntryId, Option<EntryId>)>) {
        for e in rows {
            out.push((e.id.clone(), parent.cloned()));
            walk(&e.group, Some(&e.id), out);
        }
    }
    let mut out = Vec::new();
    walk(tree, None, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use crate::fiber::FiberState;
    use crate::kernel::tests::{row, TreeHarness};

    #[tokio::test]
    async fn plugin_change_rebuilds_with_a_new_uid() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one")]).await;
        let before = h.uid("a");
        h.apply(vec![row("a").plugin("two")]).await;
        assert_ne!(
            h.uid("a"),
            before,
            "a plugin change is a rebuild, not a reload"
        );
        assert_eq!(h.state("a"), FiberState::Active);
        assert!(
            h.trace.index_of("a/one:unwind").unwrap() < h.trace.index_of("a/two:apply").unwrap()
        );
    }

    #[tokio::test]
    async fn id_change_rebuilds() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one")]).await;
        h.apply(vec![row("b").plugin("one")]).await;
        assert!(h.fiber("a").is_none(), "the old id is gone");
        assert_eq!(h.state("b"), FiberState::Active);
        assert_eq!(h.trace.count("a/one:unwind"), 1);
    }

    #[tokio::test]
    async fn material_config_diff_reloads() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one").cfg("who", "world")])
            .await;
        h.trace.push("--");
        h.apply(vec![row("a").plugin("one").cfg("who", "moon")])
            .await;
        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        assert!(
            t[base..].iter().any(|e| e == "a/one:unwind"),
            "a material config diff reloads: {t:?}"
        );
        assert_eq!(h.state("a"), FiberState::Active);
    }

    #[tokio::test]
    async fn immaterial_config_diff_does_not_reload() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one").cfg("who", "world")])
            .await;
        let uid = h.uid("a");
        h.trace.push("--");
        // `log_level` is the immaterial field the test factory absorbs.
        h.apply(vec![row("a")
            .plugin("one")
            .cfg("who", "world")
            .cfg("log_level", "debug")])
            .await;
        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        assert!(
            !t[base..].iter().any(|e| e == "a/one:unwind"),
            "an immaterial diff must be absorbed live: {t:?}"
        );
        assert_eq!(h.uid("a"), uid);
    }

    #[tokio::test]
    async fn config_is_handed_over_even_when_immaterial() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one").cfg("who", "world")])
            .await;
        h.apply(vec![row("a")
            .plugin("one")
            .cfg("who", "world")
            .cfg("log_level", "debug")])
            .await;
        assert_eq!(
            h.trace.count("a/one:reconfigure"),
            1,
            "the plugin decides materiality, so it is always handed the new config"
        );
    }

    #[tokio::test]
    async fn disabled_true_unloads_and_cascades() {
        let h = TreeHarness::new();
        h.apply(vec![row("p").plugin("one").child(row("c").plugin("one"))])
            .await;
        assert_eq!(h.state("c"), FiberState::Active);
        h.apply(vec![row("p")
            .plugin("one")
            .disabled(true)
            .child(row("c").plugin("one"))])
            .await;
        assert_eq!(h.state("p"), FiberState::Inactive);
        assert!(
            h.fiber("c").is_none(),
            "a disabled parent cascades to its group children"
        );
    }

    #[tokio::test]
    async fn disabled_false_reloads() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one").disabled(true)]).await;
        assert_eq!(h.state("a"), FiberState::Inactive);
        h.apply(vec![row("a").plugin("one")]).await;
        assert_eq!(h.state("a"), FiberState::Active);
    }

    #[tokio::test]
    async fn isolate_change_reassigns_realm_and_reloads() {
        let h = TreeHarness::new();
        h.apply(vec![
            row("p").plugin("provider"),
            row("c").plugin("one").inject(&["greeting"]),
        ])
        .await;
        assert_eq!(h.state("c"), FiberState::Active);
        h.trace.push("--");
        h.apply(vec![
            row("p").plugin("provider"),
            row("c")
                .plugin("one")
                .inject(&["greeting"])
                .isolate("greeting", "session-a"),
        ])
        .await;
        assert_eq!(h.realm("c", "greeting").as_deref(), Some("session-a"));
        // Not just the config echo: the realm map the resolver consults moved too, and the
        // fiber recaptured a committed view under it.
        assert_eq!(h.fiber_realm("c", "greeting").as_deref(), Some("session-a"));
        assert!(h.fiber("c").unwrap().committed_view().is_some());
        assert_eq!(h.state("c"), FiberState::Active);
        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        assert!(
            t[base..].iter().any(|e| e == "c/one:unwind"),
            "an isolate change reloads: {t:?}"
        );
    }

    #[tokio::test]
    async fn inject_change_reloads_only_when_a_target_differs() {
        let h = TreeHarness::new();
        h.apply(vec![
            row("p").plugin("provider"),
            row("c").plugin("one").inject(&["greeting"]),
        ])
        .await;
        h.trace.push("--");
        // Adding an OPTIONAL key nothing provides changes the declaration but no resolved target.
        h.apply(vec![
            row("p").plugin("provider"),
            row("c")
                .plugin("one")
                .inject(&["greeting"])
                .inject_optional(&["nobody-provides-this"]),
        ])
        .await;
        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        assert!(
            !t[base..].iter().any(|e| e == "c/one:unwind"),
            "no resolved ProviderUid moved, so no reload: {t:?}"
        );
    }

    #[tokio::test]
    async fn quiescent_state_is_order_independent() {
        let target = [
            row("p").plugin("provider"),
            row("c").plugin("one").inject(&["greeting"]),
            row("d").plugin("one").disabled(true),
        ];
        let mut states = Vec::new();
        for order in [vec![0usize, 1, 2], vec![2, 0, 1], vec![1, 2, 0]] {
            let h = TreeHarness::new();
            h.apply(vec![row("c").plugin("one").inject(&["greeting"])])
                .await;
            let permuted: Vec<_> = order.iter().map(|i| target[*i].clone()).collect();
            h.apply(permuted).await;
            // The WHOLE quiescent state, not three enum values: id, plugin, disabled, unmet
            // keys, provided keys and realms of every row. `uid` is excluded because it is
            // allocated per harness and carries no cross-run meaning.
            let mut rows: Vec<_> = h
                .kernel
                .rows_snapshot()
                .into_iter()
                .map(|r| {
                    (
                        r.id, r.plugin, r.state, r.disabled, r.unmet, r.provides, r.realms,
                    )
                })
                .collect();
            rows.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
            states.push(rows);
        }
        assert_eq!(
            states[0], states[1],
            "row order changed the quiescent state"
        );
        assert_eq!(
            states[1], states[2],
            "row order changed the quiescent state"
        );
        let by_id: Vec<(&str, FiberState)> =
            states[0].iter().map(|r| (r.0.as_str(), r.2)).collect();
        assert_eq!(
            by_id,
            vec![
                ("c", FiberState::Active),
                ("d", FiberState::Inactive),
                ("p", FiberState::Active)
            ]
        );
    }

    #[tokio::test]
    async fn removed_row_disposes() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one"), row("b").plugin("one")])
            .await;
        h.apply(vec![row("a").plugin("one")]).await;
        assert!(h.fiber("b").is_none());
        assert_eq!(h.trace.count("b/one:unwind"), 1);
        assert_eq!(h.state("a"), FiberState::Active);
    }
}
