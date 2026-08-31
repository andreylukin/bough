//! §0.2 runtime invariant for `bough-plugin-commands`:
//!
//! **A name is unique per scope, and every dispatch resolves to a command that was registered at
//! the moment it was dispatched.** A dispatch against a name whose registering row had already
//! unloaded is the violation the "registrations are effects" rule exists to prevent.
//!
//! The check is a fold over this crate's own observed registration/dispatch stream. WP-1 owns the
//! recorder and the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// One observed moment of the registry's life.
#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    Registered {
        name: String,
        scope: String,
    },
    Unregistered {
        name: String,
        scope: String,
    },
    Dispatched {
        name: String,
        scope: String,
        resolved: bool,
    },
}

/// The recorded stream. This crate's row clears it on unload, so a reload never reads as a
/// violation of its predecessor, and the cap keeps a week-long session from growing a leak.
static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

/// How many observations are kept. Bounded on purpose (§0.2), like `agents`' recorder.
const CAP: usize = 4096;

/// Record one moment. Called from `register`'s effect and from `dispatch`.
pub fn record(obs: Obs) {
    let mut seen = SEEN.lock();
    seen.push(obs);
    // Dropping the OLDEST observations can only hide a registration whose command is long gone;
    // it can never invent one, so the check stays sound under the bound.
    if seen.len() > CAP {
        let drop = seen.len() - CAP;
        seen.drain(..drop);
    }
}

/// Drop the recorded stream. Test setup, and this row's own unload.
pub fn clear() {
    SEEN.lock().clear();
}

/// Everything recorded so far.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// PURE: the fold the check runs.
///
/// Two things are asserted, and nothing else: a name is unique per scope while it is registered,
/// and every dispatch that RESOLVED resolved to a command registered at that moment.
pub fn check_stream(seen: &[Obs]) -> Result<(), String> {
    let mut live: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    for obs in seen {
        match obs {
            Obs::Registered { name, scope } => {
                if !live.insert((name.clone(), scope.clone())) {
                    return Err(format!(
                        "command `{name}` is registered twice in scope `{scope}`: which one runs \
                         would depend on load order"
                    ));
                }
            }
            Obs::Unregistered { name, scope } => {
                live.remove(&(name.clone(), scope.clone()));
            }
            Obs::Dispatched {
                name,
                scope,
                resolved,
            } => {
                if *resolved && !live.contains(&(name.clone(), scope.clone())) {
                    return Err(format!(
                        "`{name}` dispatched in scope `{scope}` resolved to a command that was \
                         not registered at that moment"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "commands_resolve_to_a_registered_command",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    check_stream(&seen()).map_err(|detail| InvariantViolation {
        invariant: "commands_resolve_to_a_registered_command",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(name: &str, scope: &str) -> Obs {
        Obs::Registered {
            name: name.into(),
            scope: scope.into(),
        }
    }

    #[test]
    fn a_planted_duplicate_name_in_one_scope_is_reported() {
        let seen = vec![reg("focus", "global"), reg("focus", "global")];
        assert!(check_stream(&seen)
            .unwrap_err()
            .contains("registered twice"));
        // The same name in two DIFFERENT scopes is the shadowing rule, not a violation.
        assert_eq!(
            check_stream(&[reg("focus", "global"), reg("focus", "agent:sol")]),
            Ok(())
        );
    }

    #[test]
    fn a_dispatch_of_an_unregistered_name_is_reported() {
        let seen = vec![Obs::Dispatched {
            name: "focus".into(),
            scope: "global".into(),
            resolved: true,
        }];
        assert!(check_stream(&seen).unwrap_err().contains("not registered"));
        // Unregistering first is the same violation: the row unloaded, the command is gone.
        let seen = vec![
            reg("focus", "global"),
            Obs::Unregistered {
                name: "focus".into(),
                scope: "global".into(),
            },
            Obs::Dispatched {
                name: "focus".into(),
                scope: "global".into(),
                resolved: true,
            },
        ];
        assert!(check_stream(&seen).unwrap_err().contains("not registered"));
    }

    #[test]
    fn an_unknown_name_that_resolved_to_nothing_is_not_a_violation() {
        let seen = vec![Obs::Dispatched {
            name: "fcus".into(),
            scope: "global".into(),
            resolved: false,
        }];
        assert_eq!(check_stream(&seen), Ok(()));
    }
}
