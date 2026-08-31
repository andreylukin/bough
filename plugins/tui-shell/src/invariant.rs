//! §0.2 runtime invariant for `bough-plugin-tui-shell`:
//!
//! **Every registered pane's owner row is still ACTIVE, and no two panes share an id.** A pane
//! outliving the row that registered it is exactly the failure "registrations are effects"
//! forbids, and it is what the SWAP gate would otherwise hide.

use std::collections::BTreeSet;

use bough_kernel::{Cadence, Context, EntryId, FiberState, InvariantSpec, InvariantViolation};

const NAME: &str = "every_pane_has_a_live_owner_and_a_unique_id";

/// PURE: the check, over the live pane list and the set of active row ids.
pub fn check_panes(panes: &[crate::pane::PaneInfo], active_rows: &[EntryId]) -> Result<(), String> {
    let active: BTreeSet<&EntryId> = active_rows.iter().collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for p in panes {
        if !seen.insert(p.id.to_string()) {
            return Err(format!(
                "two panes share the id `{}`; layout order and hit routing both key on it",
                p.id
            ));
        }
        if !active.contains(&p.owner) {
            return Err(format!(
                "pane `{}` is registered by row `{}`, which is not Active; a registration is an \
                 effect and must not outlive its row",
                p.id, p.owner
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    // The shell's own handle: read through the live store, because the check runs on the row that
    // PROVIDES the key and a committed view of one's own provision is not what is asserted here.
    let Some(tui) = ctx.peek_live::<crate::Tui>() else {
        // No shell means no panes; nothing to violate.
        return Ok(());
    };
    let Some(kernel) = ctx.kernel() else {
        return Ok(());
    };
    let active = active_rows(&kernel.snapshot().rows);
    check_panes(&tui.panes(), &active).map_err(fail)
}

/// Every ACTIVE row id in a tree snapshot, children included.
fn active_rows(rows: &[bough_kernel::RowSnapshot]) -> Vec<EntryId> {
    let mut out = Vec::new();
    for r in rows {
        if r.state == FiberState::Active {
            out.push(r.id.clone());
        }
        out.extend(active_rows(&r.children));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{PaneInfo, Slot, SlotSize};
    use crate::PaneId;

    fn info(id: &str, owner: &str) -> PaneInfo {
        PaneInfo {
            id: PaneId::new(id),
            slot: Slot::Main,
            order: 0,
            size: SlotSize::Fill(1),
            title: id.to_string(),
            focusable: true,
            owner: EntryId::new(owner),
        }
    }

    #[test]
    fn a_pane_whose_owner_row_is_gone_is_a_violation() {
        let panes = vec![info("focus", "tui.focus")];
        let err = check_panes(&panes, &[EntryId::new("tui")]).expect_err("the owner is not active");
        assert!(err.contains("tui.focus"), "{err}");
    }

    #[test]
    fn two_panes_sharing_an_id_is_a_violation() {
        let panes = vec![info("focus", "a"), info("focus", "b")];
        let err = check_panes(&panes, &[EntryId::new("a"), EntryId::new("b")])
            .expect_err("the ids collide");
        assert!(err.contains("share the id"), "{err}");
    }

    #[test]
    fn a_clean_registry_holds() {
        let panes = vec![info("strip", "tui.strip"), info("focus", "tui.focus")];
        check_panes(
            &panes,
            &[EntryId::new("tui.strip"), EntryId::new("tui.focus")],
        )
        .expect("every owner is active and every id is unique");
    }
}
