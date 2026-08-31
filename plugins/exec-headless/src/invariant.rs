//! No runtime invariant: `exec-headless` is a SURFACE row. It sends one message, waits for idle
//! and prints — every relation it could break belongs to `agents`, `agent-loop` or the ledger,
//! and all three run their own checks over the session it drives (§0.2).
