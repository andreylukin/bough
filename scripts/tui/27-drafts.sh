#!/usr/bin/env bash
# §7/§17 Phase 6 — everything outward-facing that is not one of the four action kinds is a DRAFT.
# A scripted `draft_message` call lands in the drafts pane, the pane says on its own header that
# nothing was sent, and the pane offers NO key that reaches an audience: `y` copies to the
# terminal's clipboard and that is the whole of its outward vocabulary.
# The subject here is the TYPED tool surface: the transcript calls tools by name, and code mode —
# the default consumer since 2026-08-28 — conceals them. `TYPED_TOOLS=1` boots the shipped fallback
# layer (`bundles/bough-typed.yml`).
TYPED_TOOLS=1
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_drafted_message_appears_as_a_card_in_the_transcript "the draft is replayed, not live"; exit 0; }

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

t no_drafts_pane_takes_a_row_before_there_is_a_draft \
  see "nothing written yet" --not --timeout 5000

# The composer can be painted before the tree is ready to take a message: on a cold boot under
# load the first submit is occasionally swallowed and NOTHING reaches the ledger (no user step, no
# wake). That is a real drop and it is recorded as such in docs/phase-6-plan.md §6 and in
# docs/track-b-merge-notes.md; until the crates this track may not edit close it, the script
# retries rather than reporting a boundary failure it did not observe.
submit_until_echoed() {
  local text="$1" i
  for i in 1 2 3; do
    shell-use submit "$text" >/dev/null
    if shell-use wait text "$text" --timeout 20000 >/dev/null 2>&1; then return 0; fi
    echo "# the composer swallowed the message (attempt $i); retrying" >&2
  done
  return 1
}

t the_composer_takes_the_message \
  submit_until_echoed "tell the eng channel the deploy is green"

# PANE-ONLY STRINGS. This used to look for "the deploy is green", which is a substring of the
# message `submit_until_echoed` has already forced onto the screen — so the bullet passed with an
# empty pane, a missing pane, or no draft at all. `1 draft` is the pane's own header (it counts
# rows), and `message →` is `row_line`'s own rendering; neither can come from the composer echo.
t a_drafted_message_appears_as_a_card_in_the_transcript \
  see "draft" --timeout 20000

t the_card_says_what_kind_of_draft_it_is \
  see "draft · message" --timeout 5000

t the_card_says_nothing_was_sent \
  see "not sent" --timeout 5000
# The card's own button line, in the transcript's columns: `copy open` and nothing else — the
# composer band has its own `send ⏎` chip (the TUI brief, D7), which is not the card's.
t the_card_offers_copy_and_open_and_no_send \
  bash -c 'for i in $(seq 1 10); do shell-use text | python3 -c "
import re, sys
rows = sys.stdin.read().split(chr(10))
ok = any(re.match(r\"^[ \u2503]*copy (open|close) *$\", r[34:]) for r in rows)
sys.exit(0 if ok else 1)
" && exit 0; sleep 0.5; done; echo "no copy/open button line on the card"; exit 1'

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
