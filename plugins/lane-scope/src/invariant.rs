//! §0.2: **No runtime invariant.** This row contributes a section and a restriction per lane and
//! owns no durable relation and no event stream. The properties worth checking — most-specific
//! section wins, `restrict` composes as an intersection — belong to `plugins/projection` and
//! `plugins/tools`, which own those registries and already state them. Duplicating them here would
//! assert the same rule twice and let the two copies disagree.
//!
//! The row's behaviour is pinned by `tests/scope.rs` (V6).
