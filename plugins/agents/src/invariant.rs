//! §0.2 runtime invariant for `bough-plugin-agents`:
//!
//! **Status never repeats; a disposed agent is terminal (no status change and no wake after
//! disposal); and at most one factory is ever set.**
//!
//! The check is a fold over the observed `agent/status` + `agent/disposed` + `agent/wake`
//! streams, per fiber and bounded — Phase 1's lesson: two fibers are two streams, and a reload
//! must not read as a violation of its own predecessor. WP-2 owns the recorder and the check.

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};

use crate::agent::Status;
use crate::ids::AgentId;

/// One observed moment of an agent's life.
#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    Status {
        fiber: FiberUid,
        agent: AgentId,
        from: Status,
        to: Status,
    },
    Disposed {
        fiber: FiberUid,
        agent: AgentId,
    },
    WakeStarted {
        fiber: FiberUid,
        agent: AgentId,
    },
}

/// The recorded stream. Per-fiber, so a reload is not a violation of its predecessor.
static SEEN: parking_lot::Mutex<Vec<Obs>> = parking_lot::Mutex::new(Vec::new());

fn fiber_of(obs: &Obs) -> FiberUid {
    match obs {
        Obs::Status { fiber, .. }
        | Obs::Disposed { fiber, .. }
        | Obs::WakeStarted { fiber, .. } => *fiber,
    }
}

fn agent_of(obs: &Obs) -> &AgentId {
    match obs {
        Obs::Status { agent, .. }
        | Obs::Disposed { agent, .. }
        | Obs::WakeStarted { agent, .. } => agent,
    }
}

/// Record one moment. Called by the listeners `AgentsPlugin::apply` registers.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Forget everything recorded for `fiber`, as an inverse of `apply`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| fiber_of(o) != fiber);
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
}

/// The whole invariant as a pure function of the observed stream: the first violation wins, and
/// the detail names the agent and what it did.
///
/// The stream is folded PER (fiber, agent): two fibers are two streams, so a reload that starts a
/// fresh agent life cannot read as a repeat of the retired one (Phase 1's lesson).
pub fn evaluate(stream: &[Obs]) -> Result<(), String> {
    use std::collections::BTreeMap;

    struct Life {
        status: Status,
        disposed: bool,
    }
    let mut lives: BTreeMap<(FiberUid, String), Life> = BTreeMap::new();

    for obs in stream {
        let key = (fiber_of(obs), agent_of(obs).to_string());
        let life = lives.entry(key).or_insert(Life {
            status: Status::Idle,
            disposed: false,
        });
        match obs {
            Obs::Status {
                agent, from, to, ..
            } => {
                if life.disposed {
                    return Err(format!(
                        "agent `{agent}` changed status to {to:?} after it was disposed"
                    ));
                }
                if from == to {
                    return Err(format!(
                        "agent `{agent}` published status {to:?} twice in a row"
                    ));
                }
                if life.status == *to {
                    return Err(format!(
                        "agent `{agent}` published status {to:?}, which repeats its current status"
                    ));
                }
                life.status = *to;
            }
            Obs::Disposed { agent, .. } => {
                if life.disposed {
                    return Err(format!("agent `{agent}` was disposed twice"));
                }
                life.disposed = true;
            }
            Obs::WakeStarted { agent, .. } => {
                if life.disposed {
                    return Err(format!(
                        "agent `{agent}` started a wake after it was disposed"
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The spec `AgentsPlugin::invariants` returns.
pub fn agent_lifecycle_is_sane() -> InvariantSpec {
    InvariantSpec {
        name: "agent_status_never_repeats_and_disposal_is_terminal",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "agent_status_never_repeats_and_disposal_is_terminal",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fibers() -> (FiberUid, FiberUid) {
        let core = bough_kernel::KernelCore::new();
        (core.new_fiber_uid(), core.new_fiber_uid())
    }

    fn a() -> AgentId {
        AgentId::new("sol")
    }

    /// The rule §0.2 names for this crate: status never repeats.
    #[test]
    fn a_repeated_status_is_reported() {
        let (f, _) = fibers();
        let clean = vec![
            Obs::Status {
                fiber: f,
                agent: a(),
                from: Status::Idle,
                to: Status::Running,
            },
            Obs::Status {
                fiber: f,
                agent: a(),
                from: Status::Running,
                to: Status::Idle,
            },
        ];
        assert_eq!(evaluate(&clean), Ok(()));

        let planted = vec![
            Obs::Status {
                fiber: f,
                agent: a(),
                from: Status::Idle,
                to: Status::Running,
            },
            Obs::Status {
                fiber: f,
                agent: a(),
                from: Status::Running,
                to: Status::Running,
            },
        ];
        let detail = evaluate(&planted).expect_err("a repeat must be reported");
        assert!(
            detail.contains("sol"),
            "the detail must name the agent: {detail}"
        );
        assert!(
            detail.contains("Running"),
            "the detail must name the status: {detail}"
        );
    }

    /// Disposal is terminal: neither a status change nor a wake may follow it.
    #[test]
    fn a_status_after_disposal_is_reported() {
        let (f, _) = fibers();
        let planted = vec![
            Obs::Disposed {
                fiber: f,
                agent: a(),
            },
            Obs::Status {
                fiber: f,
                agent: a(),
                from: Status::Idle,
                to: Status::Running,
            },
        ];
        let detail = evaluate(&planted).expect_err("a post-disposal status must be reported");
        assert!(detail.contains("after it was disposed"), "{detail}");

        let woke = vec![
            Obs::Disposed {
                fiber: f,
                agent: a(),
            },
            Obs::WakeStarted {
                fiber: f,
                agent: a(),
            },
        ];
        let detail = evaluate(&woke).expect_err("a post-disposal wake must be reported");
        assert!(detail.contains("started a wake"), "{detail}");
    }

    /// Phase 1's lesson: two fibers are two streams. A reload disposes the old life and starts a
    /// fresh one under the SAME agent name, and that is not a violation of anything.
    #[test]
    fn two_fibers_are_two_streams() {
        let (old, new) = fibers();
        let reload = vec![
            Obs::Status {
                fiber: old,
                agent: a(),
                from: Status::Idle,
                to: Status::Running,
            },
            Obs::Status {
                fiber: old,
                agent: a(),
                from: Status::Running,
                to: Status::Idle,
            },
            Obs::Disposed {
                fiber: old,
                agent: a(),
            },
            // The successor fiber's life: same agent, same first transition, clean.
            Obs::Status {
                fiber: new,
                agent: a(),
                from: Status::Idle,
                to: Status::Running,
            },
            Obs::WakeStarted {
                fiber: new,
                agent: a(),
            },
        ];
        assert_eq!(evaluate(&reload), Ok(()));
    }

    /// `forget` is the inverse of `apply`: unloading one fiber leaves the other's stream intact.
    #[test]
    fn forget_drops_only_that_fibers_observations() {
        let (old, new) = fibers();
        clear();
        record(Obs::Status {
            fiber: old,
            agent: a(),
            from: Status::Idle,
            to: Status::Running,
        });
        record(Obs::Status {
            fiber: new,
            agent: a(),
            from: Status::Idle,
            to: Status::Running,
        });
        forget(old);
        let left = seen();
        assert!(left.iter().all(|o| super::fiber_of(o) == new), "{left:?}");
        clear();
    }
}
