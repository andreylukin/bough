//! Invariant: the no-op source ACTIVATES and reports nothing. It exists so a non-macOS deployment
//! boots with the row enabled (§0.2) instead of failing or silently skipping.

use bough_plugin_power::{PowerEvent, PowerSource};

/// A source that never fires.
pub struct NoopSource;

impl PowerSource for NoopSource {
    fn kind(&self) -> &'static str {
        "noop"
    }
    fn last(&self) -> Option<PowerEvent> {
        None
    }
}
