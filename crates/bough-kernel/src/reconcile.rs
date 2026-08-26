//! Invariant: the reconciler never calls a lifecycle method. It diffs two trees by row id and
//! writes each fiber's `target`; the drivers converge. That is what makes the quiescent state a
//! function of the FINAL tree alone, independent of the order the diff was walked (§0.3, §0.5).

use crate::config::{Composition, Entry};
use crate::fiber::{EntryId, FiberUid};

/// The only thing a diff produces.
#[derive(Clone, Debug, PartialEq)]
pub enum TargetWrite {
    /// A row present only in the new tree.
    Create { id: EntryId },
    /// `id` or `plugin` changed: dispose the old fiber entirely and create a new one, with a new
    /// [`FiberUid`].
    Rebuild { id: EntryId, previous: FiberUid },
    /// Hand the new config to the plugin via `reconfigure`; `Applied` ⇒ nothing, `Reload` ⇒
    /// unload then load.
    Reconfigure { id: EntryId },
    /// `disabled` false→true, or the row is absent in the new tree.
    Unload { id: EntryId },
    /// `disabled` true→false; PENDING until the row's keys resolve.
    Load { id: EntryId },
    /// `isolate` or a resolved `ProviderUid` changed: unload then load.
    Reload { id: EntryId },
}

/// Per-field reconciliation, exactly as tabulated in §0.3 / plan §2.8. Pure: it reads two trees
/// and produces target writes.
pub fn diff(old: &Composition, new: &Composition) -> Vec<TargetWrite> {
    todo!("WP-3")
}

/// Per-row diff, recursing into `group`. Added children mount as effects of the parent; removed
/// children dispose.
pub fn diff_row(old: Option<&Entry>, new: Option<&Entry>, out: &mut Vec<TargetWrite>) {
    todo!("WP-3")
}
