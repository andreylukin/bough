//! Invariant: section order is a pure, total function of `(Slot, Place, SectionId)` — stable under
//! any input permutation (P1-D8).

use crate::RenderedSection;

/// Sort the rendered sections into §5's fixed order.
// `&mut Vec` rather than `&mut [_]`: the phase plan §2.7 fixes this signature, and a rung of the
// degradation ladder removes elements through it.
#[allow(clippy::ptr_arg)]
pub fn order(sections: &mut Vec<RenderedSection>) {
    // A TOTAL key: two sections can only compare equal when they carry the same SectionId at the
    // same position, which the registry already forbids. So the result never depends on the input
    // permutation, and never on the order fibers happened to activate in.
    sections.sort_by(|a, b| a.position.sort_key(&a.id).cmp(&b.position.sort_key(&b.id)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{Place, Position, SectionCites, SectionId, Slot};

    fn sec(id: &str, slot: Slot, place: Place) -> RenderedSection {
        RenderedSection {
            id: SectionId::new(id),
            position: Position { slot, place },
            title: id.to_string(),
            body: String::new(),
            cites: SectionCites::default(),
            tokens: 0,
            degraded: None,
        }
    }

    fn ids(v: &[RenderedSection]) -> Vec<&str> {
        v.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn fixed_slot_order_is_identity_pins_digest_tiers_tail_mail() {
        let mut v = vec![
            sec("mail", Slot::Mail, Place::Band),
            sec("tail", Slot::Tail, Place::Band),
            sec("digest", Slot::Digest, Place::Band),
            sec("identity", Slot::Identity, Place::Band),
            sec("tiers", Slot::Tiers, Place::Band),
            sec("pins", Slot::Pins, Place::Band),
        ];
        order(&mut v);
        assert_eq!(
            ids(&v),
            vec!["identity", "pins", "digest", "tiers", "tail", "mail"]
        );
    }

    #[test]
    fn before_precedes_the_band_and_after_follows_it() {
        let mut v = vec![
            sec("z-after", Slot::Identity, Place::After),
            sec("m-band", Slot::Identity, Place::Band),
            sec("a-before", Slot::Identity, Place::Before),
        ];
        order(&mut v);
        assert_eq!(ids(&v), vec!["a-before", "m-band", "z-after"]);

        // And the band never leaks past its own slot: an `After` of Identity still precedes
        // everything in Pins.
        let mut v = vec![
            sec("pins", Slot::Pins, Place::Before),
            sec("after-identity", Slot::Identity, Place::After),
        ];
        order(&mut v);
        assert_eq!(ids(&v), vec!["after-identity", "pins"]);
    }

    #[test]
    fn ties_break_by_section_id_not_registration_order() {
        // Same slot, same place: registration order is "b, a" and the result is "a, b".
        let mut v = vec![
            sec("b", Slot::Tail, Place::After),
            sec("a", Slot::Tail, Place::After),
        ];
        order(&mut v);
        assert_eq!(ids(&v), vec!["a", "b"]);

        let mut reversed = vec![
            sec("a", Slot::Tail, Place::After),
            sec("b", Slot::Tail, Place::After),
        ];
        order(&mut reversed);
        assert_eq!(
            ids(&reversed),
            ids(&v),
            "fiber activation order is not deterministic, so it must not reach the output"
        );
    }

    #[test]
    fn ordering_is_stable_under_shuffled_input() {
        let base = vec![
            sec("a", Slot::Identity, Place::Before),
            sec("b", Slot::Identity, Place::Band),
            sec("c", Slot::Pins, Place::Band),
            sec("d", Slot::Pins, Place::After),
            sec("e", Slot::Tiers, Place::Band),
            sec("f", Slot::Mail, Place::Before),
            sec("g", Slot::Mail, Place::Band),
        ];
        let mut expected = base.clone();
        order(&mut expected);

        // Every rotation is a different "registration order"; all of them must agree. A rotation
        // is enough: the key is total, so no permutation can disagree with another.
        for k in 0..base.len() {
            let mut v: Vec<RenderedSection> =
                base[k..].iter().chain(base[..k].iter()).cloned().collect();
            order(&mut v);
            assert_eq!(ids(&v), ids(&expected), "rotation by {k} disagreed");
        }
    }
}
