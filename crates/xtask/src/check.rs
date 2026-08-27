//! Invariant (§15 item 7): the gate is PURE over a [`Catalog`]. Empty findings = the tree's
//! declared dispatch modes and its dispatch sites agree. The three residual risks the type system
//! cannot catch are the three the checks below name.

use crate::scan::{Catalog, DispatchSite, EventDecl};

/// What the gate found. Anything here fails the gate.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// `impl EmitEvent for X { const MODE = DispatchMode::Serial; }` — the catalog surface and the
    /// dispatcher would disagree, silently. The mismatch the compiler CANNOT catch.
    ModeOverrideDisagreesWithTrait { decl: EventDecl },
    /// Two types declare the same `NAME` under different modes.
    NameDeclaredTwiceWithDifferentModes {
        name: String,
        a: EventDecl,
        b: EventDecl,
    },
    /// `.waterfall::<X>()` where `X` declares Emit (a type impl'ing two event traits).
    DispatchModeDiffersFromDeclaration {
        site: DispatchSite,
        decl: EventDecl,
    },
    /// The same, for a listener registration.
    ListenModeDiffersFromDeclaration {
        site: DispatchSite,
        decl: EventDecl,
    },
    /// A dispatch site whose type declares no event trait anywhere in the tree.
    UndeclaredDispatch { site: DispatchSite },
}

/// PURE: the five checks. Empty = the gate passes.
///
/// WP-6.
pub fn check(c: &Catalog) -> Vec<Finding> {
    let _ = c;
    todo!("WP-6: the five checks over the catalog")
}
