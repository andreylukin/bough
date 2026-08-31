//! No runtime invariant: `llm-replay` is a test PROVIDER that answers from a recorded file. Its
//! only relation — "a stream ends once" — is the `llm` seam's own invariant, which runs over its
//! output already (§0.2).
