//! No runtime invariant: this is a CONSUMER. Each tool does nothing but call
//! `ActionsHandle::execute`, so the journal relation it could violate is the `actions` seam's own
//! invariant, checked over exactly this row's output (§0.2).
