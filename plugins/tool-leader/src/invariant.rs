//! §0.2: **No runtime invariant.** This row registers two tools and holds no data relation and no
//! event stream of its own. What could be checked about it — that the tools are visible to the
//! target and to nobody else — is a REGISTRY property, and `plugins/tools` already owns the
//! most-specific-wins rule and its invariant; asserting it a second time here would be a test, not
//! an invariant. The row's behaviour is pinned by `tests/tools.rs` and by the phase's SWAP test.
