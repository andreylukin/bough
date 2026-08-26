//! §0.2 runtime invariant for `bough-plugin-tool-mcp`:
//!
//! **A tool name is registered for a server only between that server's `Added` and its `Removed`.**
//!
//! The stream is per-LIFE, keyed by the recording fiber: a reload keeps the `FiberUid` and the
//! fiber's observations are forgotten when it unloads, or a reload would flag itself (§0.3).
//! Restated as a fold over the observed registration stream: no name is registered twice without
//! a withdrawal in between, and a withdrawal always follows a registration. This is "the set shown
//! and the set callable are the same set" as a stream property.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

/// Bounded on purpose: an invariant that grows without limit is a leak, not a check.
const CAP: usize = 4096;

/// One observed registry change.
#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    Registered { server: String, name: String },
    Withdrawn { server: String },
}

type Stream = Mutex<Vec<(FiberUid, Obs)>>;

fn stream() -> &'static Stream {
    static S: OnceLock<Stream> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Record one change, against the fiber that made it.
pub fn record(fiber: FiberUid, obs: Obs) {
    let mut s = stream().lock();
    s.push((fiber, obs));
    let len = s.len();
    if len > CAP {
        s.drain(0..len - CAP);
    }
}

/// Forget everything one fiber recorded. Deferred by `apply`, so a reload starts clean.
pub fn forget(fiber: FiberUid) {
    stream().lock().retain(|(f, _)| *f != fiber);
}

/// Forget everything. Only tests use it: several kernels in one process mint colliding
/// `FiberUid`s, so a test that asserts on violations clears the stream first.
pub fn reset() {
    stream().lock().clear();
}

/// What one fiber has recorded.
pub fn observed(fiber: FiberUid) -> Vec<Obs> {
    stream()
        .lock()
        .iter()
        .filter(|(f, _)| *f == fiber)
        .map(|(_, o)| o.clone())
        .collect()
}

/// The pure half: the first violation in `obs`, if any.
pub fn violation(obs: &[Obs]) -> Option<String> {
    let mut live: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for o in obs {
        match o {
            Obs::Registered { server, name } => {
                let names = live.entry(server.clone()).or_default();
                if names.contains(name) {
                    return Some(format!(
                        "`{name}` was registered twice for `{server}` with no withdrawal between"
                    ));
                }
                names.push(name.clone());
            }
            Obs::Withdrawn { server } => {
                live.remove(server);
            }
        }
    }
    None
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: "tool-mcp/registrations-track-the-server-set",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| {
            Box::pin(async move {
                let obs = observed(ctx.fiber_uid());
                match violation(&obs) {
                    None => Ok(()),
                    Some(detail) => Err(InvariantViolation {
                        invariant: "tool-mcp/registrations-track-the-server-set",
                        plugin: crate::PLUGIN_NAME,
                        entry: ctx.entry_id().clone(),
                        detail,
                    }),
                }
            })
        },
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(server: &str, name: &str) -> Obs {
        Obs::Registered {
            server: server.into(),
            name: name.into(),
        }
    }

    #[test]
    fn a_register_withdraw_register_cycle_is_clean() {
        assert_eq!(
            violation(&[
                reg("fixture", "mcp__fixture__echo"),
                Obs::Withdrawn {
                    server: "fixture".into()
                },
                reg("fixture", "mcp__fixture__echo"),
            ]),
            None
        );
    }

    #[test]
    fn registering_the_same_name_twice_without_a_withdrawal_is_a_violation() {
        let v = violation(&[
            reg("fixture", "mcp__fixture__echo"),
            reg("fixture", "mcp__fixture__echo"),
        ]);
        assert!(v.unwrap().contains("twice"));
    }
}
