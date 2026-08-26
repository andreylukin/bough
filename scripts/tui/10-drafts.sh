#!/usr/bin/env bash
# §7/§17 Phase 6 — everything outward-facing that is not one of the four action kinds is a DRAFT.
# A scripted `draft_message` call lands in the drafts pane, the pane says on its own header that
# nothing was sent, and the pane offers NO key that reaches an audience: `y` copies to the
# terminal's clipboard and that is the whole of its outward vocabulary.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_drafted_message_appears_in_the_drafts_pane "the draft is replayed, not live"; exit 0; }

# The transcript, written here rather than in `fixtures/`: it is this bullet's alone, and the
# suite's shared transcripts are ordered for V1-V4.
DRAFTS_PATCH="$HOME_DIR/drafts.patch.yml"
cat > "$DRAFTS_PATCH" <<'YML'
entries:
  llm.anthropic:
    plugin: llm-replay
    config:
      strict: true
      models: "*"
      rounds:
        - chunks:
            - type: tool_call
              id: call-draft-1
              name: draft_message
              input:
                audience: "slack:#eng"
                subject: "the deploy is green"
                body: "shipped the collector sweep; nothing to do"
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "drafted it for you" }
            - { type: end, stop: end_turn }
        - chunks:
            - { type: text, text: "drafted it for you" }
            - { type: end, stop: end_turn }
        - chunks:
            - { type: text, text: "drafted it for you" }
            - { type: end, stop: end_turn }
YML

tui_open
tui_start "$DRAFTS_PATCH"

t the_drafts_pane_is_on_screen_from_boot \
  see "drafts" --timeout 20000

shell-use submit "tell the eng channel the deploy is green"
shell-use wait idle --timeout 30000

t a_drafted_message_appears_in_the_drafts_pane \
  see "the deploy is green" --timeout 20000

t the_pane_says_nothing_was_sent \
  see "NOT sent" --timeout 5000

t the_audience_is_shown_so_andrey_knows_where_it_would_have_gone \
  see "slack:#eng" --timeout 5000

# The model was told the act is finished. If the boundary block or the tool description failed,
# the loop would keep hunting for a send and the wake would not close.
t the_model_treated_the_draft_as_the_finished_act \
  see "drafted it for you" --timeout 20000

# The step is durable and model-visible: §0.2's model-visible ⟺ ledgered, checked outside the
# process.
t the_draft_is_a_ledgered_step \
  expect_steps "draft/message" 1

tui_quit
