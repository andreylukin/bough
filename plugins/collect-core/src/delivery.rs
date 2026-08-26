//! Invariant: a collected item becomes CITED mail by construction. `delivery_of` is PURE and
//! always produces at least one cite — the item's own ref — so no collector can deliver an
//! uncitable claim (§0.2, §3).

use bough_plugin_agents::Delivery;

use crate::Collected;

/// PURE: one collected item becomes one [`Delivery`], cited by construction. WP-2.
pub fn delivery_of(item: &Collected, collector: &str) -> Delivery {
    let _ = (item, collector);
    todo!("WP-2: Sender::System(collector), cite the item's ref + url, carry `refs` for the router")
}
