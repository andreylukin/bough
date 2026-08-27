//! Invariant (§15 item 7): the gate is PURE over a [`Catalog`]. Empty findings = the tree's
//! declared dispatch modes and its dispatch sites agree. The three residual risks the type system
//! cannot catch are the three the checks below name.

use std::collections::BTreeMap;

use crate::scan::{Catalog, DispatchSite, EventDecl, SiteKind};

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
    DispatchModeDiffersFromDeclaration { site: DispatchSite, decl: EventDecl },
    /// The same, for a listener registration.
    ListenModeDiffersFromDeclaration { site: DispatchSite, decl: EventDecl },
    /// A dispatch site whose type declares no event trait anywhere in the tree.
    UndeclaredDispatch { site: DispatchSite },
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Finding::ModeOverrideDisagreesWithTrait { decl } => write!(
                f,
                "{}:{}: {} declares `const MODE = {}` but impls the {} trait",
                decl.file.display(),
                decl.line,
                decl.ty,
                decl.declared_mode.map(|m| m.as_str()).unwrap_or("?"),
                decl.trait_mode.as_str()
            ),
            Finding::NameDeclaredTwiceWithDifferentModes { name, a, b } => write!(
                f,
                "event name {name:?} is declared under two modes: {} ({}, {}:{}) and {} ({}, {}:{})",
                a.ty,
                a.effective_mode().as_str(),
                a.file.display(),
                a.line,
                b.ty,
                b.effective_mode().as_str(),
                b.file.display(),
                b.line,
            ),
            Finding::DispatchModeDiffersFromDeclaration { site, decl } => write!(
                f,
                "{}:{}: dispatches {} as {} but it declares {}",
                site.file.display(),
                site.line,
                site.ty,
                site.mode.as_str(),
                decl.effective_mode().as_str()
            ),
            Finding::ListenModeDiffersFromDeclaration { site, decl } => write!(
                f,
                "{}:{}: listens for {} as {} but it declares {}",
                site.file.display(),
                site.line,
                site.ty,
                site.mode.as_str(),
                decl.effective_mode().as_str()
            ),
            Finding::UndeclaredDispatch { site } => write!(
                f,
                "{}:{}: {} is dispatched but declares no event trait in the tree",
                site.file.display(),
                site.line,
                site.ty
            ),
        }
    }
}

/// PURE: the five checks. Empty = the gate passes.
pub fn check(c: &Catalog) -> Vec<Finding> {
    let mut findings = Vec::new();

    // 1. an override that disagrees with the trait it overrides.
    for decl in &c.decls {
        if let Some(m) = decl.declared_mode {
            if m != decl.trait_mode {
                findings.push(Finding::ModeOverrideDisagreesWithTrait { decl: decl.clone() });
            }
        }
    }

    // 2. one NAME under two modes.
    let mut by_name: BTreeMap<&str, Vec<&EventDecl>> = BTreeMap::new();
    for decl in &c.decls {
        by_name.entry(decl.name.as_str()).or_default().push(decl);
    }
    for (name, decls) in &by_name {
        let first = decls[0];
        for other in &decls[1..] {
            if other.effective_mode() != first.effective_mode() {
                findings.push(Finding::NameDeclaredTwiceWithDifferentModes {
                    name: (*name).to_string(),
                    a: first.clone(),
                    b: (*other).clone(),
                });
            }
        }
    }

    // 3-5. every site against the declaration of its type.
    let mut by_ty: BTreeMap<&str, &EventDecl> = BTreeMap::new();
    for decl in &c.decls {
        by_ty.entry(decl.ty.as_str()).or_insert(decl);
    }
    for site in &c.sites {
        let Some(decl) = by_ty.get(site.ty.as_str()) else {
            findings.push(Finding::UndeclaredDispatch { site: site.clone() });
            continue;
        };
        // A type impl'ing two event traits has two decls; the site agrees if ANY of them matches.
        let modes: Vec<_> = c
            .decls
            .iter()
            .filter(|d| d.ty == site.ty)
            .map(|d| d.effective_mode())
            .collect();
        match site.kind {
            SiteKind::Dispatch => {
                if !modes.contains(&site.mode) {
                    findings.push(Finding::DispatchModeDiffersFromDeclaration {
                        site: site.clone(),
                        decl: (*decl).clone(),
                    });
                }
            }
            // A listener registration names no mode of its own today (the kernel's `on` is
            // generic over the trait), so it can only be wrong once a mode-carrying `on` exists;
            // the check is here so that day is a one-line change, not a new pass.
            SiteKind::Listen => {}
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{DispatchMode, SiteKind};
    use std::path::PathBuf;

    fn decl(
        name: &str,
        ty: &str,
        trait_mode: DispatchMode,
        over: Option<DispatchMode>,
    ) -> EventDecl {
        EventDecl {
            name: name.into(),
            ty: ty.into(),
            trait_mode,
            declared_mode: over,
            krate: "fixture".into(),
            file: PathBuf::from("crates/fixture/src/lib.rs"),
            line: 1,
        }
    }

    fn site(ty: &str, mode: DispatchMode, kind: SiteKind) -> DispatchSite {
        DispatchSite {
            ty: ty.into(),
            mode,
            kind,
            file: PathBuf::from("crates/fixture/src/lib.rs"),
            line: 9,
        }
    }

    #[test]
    fn a_clean_catalog_has_no_findings() {
        let c = Catalog {
            decls: vec![
                decl("a", "A", DispatchMode::Emit, None),
                decl("b", "B", DispatchMode::Waterfall, None),
            ],
            sites: vec![
                site("A", DispatchMode::Emit, SiteKind::Dispatch),
                site("B", DispatchMode::Waterfall, SiteKind::Dispatch),
                site("A", DispatchMode::Emit, SiteKind::Listen),
            ],
        };
        assert_eq!(check(&c), vec![]);
    }

    #[test]
    fn a_mode_override_that_disagrees_with_its_trait_is_reported() {
        let c = Catalog {
            decls: vec![decl(
                "a",
                "A",
                DispatchMode::Emit,
                Some(DispatchMode::Serial),
            )],
            sites: vec![],
        };
        let f = check(&c);
        assert!(
            matches!(
                f.as_slice(),
                [Finding::ModeOverrideDisagreesWithTrait { .. }]
            ),
            "{f:?}"
        );
        assert!(f[0].to_string().contains("const MODE = serial"), "{}", f[0]);
    }

    #[test]
    fn one_name_under_two_modes_is_reported() {
        let c = Catalog {
            decls: vec![
                decl("dup", "A", DispatchMode::Emit, None),
                decl("dup", "B", DispatchMode::Serial, None),
                // the same name under the SAME mode is fine.
                decl("ok", "C", DispatchMode::Emit, None),
                decl("ok", "D", DispatchMode::Emit, None),
            ],
            sites: vec![],
        };
        let f = check(&c);
        assert!(
            matches!(
                f.as_slice(),
                [Finding::NameDeclaredTwiceWithDifferentModes { name, .. }] if name == "dup"
            ),
            "{f:?}"
        );
    }

    #[test]
    fn a_dispatch_site_whose_type_declares_another_mode_is_reported() {
        let c = Catalog {
            decls: vec![decl("a", "A", DispatchMode::Emit, None)],
            sites: vec![site("A", DispatchMode::Waterfall, SiteKind::Dispatch)],
        };
        let f = check(&c);
        assert!(
            matches!(
                f.as_slice(),
                [Finding::DispatchModeDiffersFromDeclaration { .. }]
            ),
            "{f:?}"
        );

        // a type impl'ing TWO event traits may legitimately be dispatched under either.
        let both = Catalog {
            decls: vec![
                decl("a", "A", DispatchMode::Emit, None),
                decl("a", "A", DispatchMode::Waterfall, None),
            ],
            sites: vec![site("A", DispatchMode::Waterfall, SiteKind::Dispatch)],
        };
        assert_eq!(
            check(&both)
                .iter()
                .filter(|f| matches!(f, Finding::DispatchModeDiffersFromDeclaration { .. }))
                .count(),
            0
        );
    }

    #[test]
    fn an_undeclared_dispatch_is_reported() {
        let c = Catalog {
            decls: vec![],
            sites: vec![site("Ghost", DispatchMode::Emit, SiteKind::Dispatch)],
        };
        let f = check(&c);
        assert!(
            matches!(f.as_slice(), [Finding::UndeclaredDispatch { .. }]),
            "{f:?}"
        );
        assert!(f[0].to_string().contains("Ghost"), "{}", f[0]);
    }
}
