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

/// Record one moment. WP-1 calls this from `register`'s effect and from `dispatch`.
pub fn record(_obs: Obs) {
    todo!("WP-1")
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    todo!("WP-1")
}

/// PURE: the fold the check runs.
pub fn check_stream(_seen: &[Obs]) -> Result<(), String> {
    todo!("WP-1")
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

async fn run(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-1")
}
