//! Invariant (§9): `ask` DEGRADES TO DENY when no approver is mounted. Phase 2 mounts none, so
//! the degradation is the live path and the tests assert it — a capability nobody can service is
//! never silently granted.

use std::sync::Arc;

use bough_kernel::ServiceKey;

use crate::tool::ToolCall;

/// The optional `approval` service key. Declared here, mounted by nobody in Phase 2.
pub struct Approval;

impl ServiceKey for Approval {
    type Value = ApprovalHandle;
    const NAME: &'static str = "approval";
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct ApprovalHandle(pub Arc<dyn Approver>);

/// Whoever can answer an `ask`.
#[async_trait::async_trait]
pub trait Approver: Send + Sync + 'static {
    async fn ask(&self, call: &ToolCall, reason: &str) -> ApprovalOutcome;
}

/// The answer. There is no third value: a timeout is the approver's problem and reaches the
/// pipeline as `Deny`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ApprovalOutcome {
    Allow,
    Deny,
}
