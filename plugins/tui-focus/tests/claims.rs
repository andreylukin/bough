//! WP-7 / §16, P5-D16: ACCEPTANCE IS ANDREY'S ACT. The card's buttons decide nothing themselves —
//! they turn into the same `/accept`, `/edit`, `/reject` line the keyboard path types, so the two
//! surfaces cannot drift apart, and a build with no `claims` row simply has no such command.

use bough_plugin_tui_focus::claims::{card, claim_action_of_hit, hit_for_claim, line_for};
use bough_plugin_tui_focus::rows::ClaimState;
use bough_plugin_tui_focus::ClaimAction;
use bough_plugin_tui_shell::pane::HitId;
use bough_plugin_tui_shell::{Theme, ThemeName};

mod tests {
    use super::*;

    /// A region round-trips: minted from a claim id and an action, parsed back to the same pair.
    /// Claim ids are opaque and may carry `:` themselves, so the ACTION is split off the END.
    #[test]
    fn a_claim_region_round_trips_through_its_hit_id() {
        for action in ClaimAction::ALL {
            let id = hit_for_claim("claim:with:colons", action);
            assert_eq!(
                claim_action_of_hit(&id),
                Some(("claim:with:colons".to_string(), action))
            );
        }
        // Regions this pane did not mint are not its business.
        assert_eq!(claim_action_of_hit(&HitId::new("tool:c1")), None);
        assert_eq!(claim_action_of_hit(&HitId::new("claim::accept")), None);
        assert_eq!(claim_action_of_hit(&HitId::new("claim:c1:explode")), None);
    }

    /// The click path and the keyboard path are ONE seam: a click produces the command line, and
    /// the two that need Andrey's words (`/edit`, `/reject`) are COMPOSED for him to finish
    /// rather than run behind his back.
    #[test]
    fn each_button_becomes_the_command_the_keyboard_path_types() {
        assert_eq!(
            line_for("c1", ClaimAction::Accept, "body"),
            ("/accept c1".to_string(), true)
        );
        let (edit, run) = line_for("c1", ClaimAction::Edit, "the drafted text");
        assert_eq!(edit, "/edit c1 the drafted text");
        assert!(!run, "an edit is finished in the composer");
        let (reject, run) = line_for("c1", ClaimAction::Reject, "body");
        assert_eq!(reject, "/reject c1 ");
        assert!(!run, "a rejection needs its reason");
    }

    /// A rejected card carries its reason on screen; an accepted one says it was accepted. A card
    /// that showed only a state word would leave the proposal looking arbitrary.
    #[test]
    fn a_decided_card_says_what_happened_and_offers_no_buttons() {
        let theme = Theme::of(ThemeName::Dark);
        let text = |state: &ClaimState| -> (Vec<String>, usize) {
            let (lines, hits) = card("c1", "lane", "a new lane", "body", state, 0, 60, &theme);
            (
                lines
                    .iter()
                    .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                    .collect(),
                hits.len(),
            )
        };
        let (open, n) = text(&ClaimState::Open);
        assert_eq!(n, 3);
        assert!(open[0].contains("open"), "{open:?}");

        let (accepted, n) = text(&ClaimState::Accepted { edited: false });
        assert_eq!(n, 0);
        assert!(accepted[0].contains("accepted"), "{accepted:?}");

        let (rejected, n) = text(&ClaimState::Rejected {
            reason: "that lane already exists".into(),
        });
        assert_eq!(n, 0);
        assert!(
            rejected
                .iter()
                .any(|l| l.contains("that lane already exists")),
            "{rejected:?}"
        );
    }
}
