//! Invariant: `decide` is the ONE place this phase's ground truth lives. It is the only writer of
//! `claim/accepted` and `claim/rejected`, the only caller of `pin/set` for a requirement, and the
//! only path from a claim to `ctx.graph`. A second writer anywhere would let a claim be accepted
//! without a pin, or a lane be born without an acceptance.
//!
//! - `Accept` on a `Requirement` ⇒ `claim/accepted { edited: false }` then `pin/set`.
//! - `Edit` ⇒ the same with `edited: true` and the EDITED text pinned; the proposal step is never
//!   rewritten — the edit is a new fact citing it.
//! - `Accept` on a `Lane` ⇒ `ctx.graph.apply(Bud { agent: Some(name) })` and `agents.resume`, in
//!   one transaction: a lane claim births a ROW and a LIVE agent, or neither.
//! - `Reject` ⇒ `claim/rejected { reason }` and nothing else, ever.
//!
//! The body is WP-4's; the entry point is [`crate::ClaimsHandle::decide`].
