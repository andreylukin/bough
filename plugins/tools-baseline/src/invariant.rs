//! No runtime invariant: `tools-baseline` is a CONSUMER — six tools registered through the
//! `tools` seam. Every relation worth policing (call/result pairing, monotone guarding, model
//! ordering) belongs to the seam and is checked by its invariant over these tools' output. What
//! is specific to these six — containment under `root`, the spill locator — is pinned by
//! `plugins/tools-baseline/tests/tools.rs` (§0.2).
