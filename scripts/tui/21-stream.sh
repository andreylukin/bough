#!/usr/bin/env bash
# V6 — streaming and markdown (phase ux1 §2.6). M10 and M19 are one bug: the stream was rendered per
# NETWORK CHUNK and the chunk boundaries were durable, so a paragraph came out broken at whatever
# byte the provider happened to flush at, and half-arrived `**` rendered as literal asterisks. The
# fix is to accumulate and wrap ON PAINT and to parse markdown over the whole accumulated document.
#
# This script runs in BOTH halves and asserts different things in each: the replay half drives the
# multi-chunk fixture, whose boundaries fall mid-word on purpose; the live half asks haiku for a
# markdown answer and asserts the same no-mid-word-break property over whatever it says.
source "$(dirname "$0")/lib.sh"

tui_open

# A word that must never be split across two rows, whichever half is running: the fixture puts a
# chunk boundary inside it.
SPLIT_WORD="accumulated"

if [ -n "$BOUGH_LIVE" ]; then
  tui_start
  shell-use submit "Reply in markdown with a level-two heading, one bold word, an inline code span and a two-item list. Keep it under 80 words."
  shell-use wait idle --timeout 90000

  t a_live_haiku_answer_has_no_mid_word_break \
    bash -c '
      # No screen row may END with a hyphenless word fragment that the next row completes: the
      # renderer wraps at word boundaries or it does not wrap at all.
      shell-use text | python3 -c "
import sys
rows = [r.rstrip() for r in sys.stdin.read().split(chr(10))]
for y in range(len(rows) - 1):
    a = rows[y]
    if a.endswith(chr(45)):
        sys.exit(\"row %d ends in a bare hyphen: a word was split\" % y)
"
    '

  t a_live_answer_shows_no_literal_markdown_markers \
    bash -c '
      shell-use text | grep -q "\*\*" && { echo "literal ** on screen: the markers were not parsed"; exit 1; }
      exit 0
    '

  skip a_multi_chunk_replay_has_no_mid_word_break "the live half has no scripted chunk boundaries"
  skip the_same_answer_renders_identically_after_a_relaunch "the live half cannot replay the same answer"
  tui_quit
  exit 0
fi

tui_start "$REPO_ROOT/scripts/tui/fixtures/markdown.patch.yml"
shell-use submit "explain the renderer"
shell-use wait idle --timeout 40000

t the_answer_landed see "$SPLIT_WORD" --timeout 20000

t a_multi_chunk_replay_has_no_mid_word_break \
  bash -c '
    # The fixture splits `accumulated` between two chunks. If a chunk boundary can become a line
    # break, the word is on two rows and no single row carries it whole.
    shell-use text | grep -q "'"$SPLIT_WORD"'" || {
      echo "no row carries '"$SPLIT_WORD"' whole: the chunk boundary became a line break"; exit 1; }
    # Same for the code span and the fenced identifier, whose boundaries are also mid-token.
    shell-use text | grep -q "measure_cols" || { echo "measure_cols was split across rows"; exit 1; }
    shell-use text | grep -q "md::document" || { echo "the fenced line was split across rows"; exit 1; }
    exit 0
  '

t the_capabilities_answer_shows_no_literal_markers \
  bash -c '
    shell-use text | grep -q "\*\*accum" && { echo "literal ** on screen"; exit 1; }
    shell-use text | grep -q "^## " && { echo "a literal ## heading marker on screen"; exit 1; }
    exit 0
  '

t every_block_kind_is_on_screen \
  bash -c '
    see "What the renderer promises" --timeout 10000 || { echo "no heading"; exit 1; }
    see "first item" --timeout 8000 || { echo "no list"; exit 1; }
    see "model" --timeout 8000 || { echo "no table"; exit 1; }
    see "fn wrap" --timeout 8000 || { echo "no fenced block"; exit 1; }
  '

t the_list_keeps_a_hanging_indent \
  bash -c '
    shell-use text | python3 -c "
import sys
rows = [r for r in sys.stdin.read().split(chr(10))]
for y, row in enumerate(rows):
    if \"first item\" in row:
        start = row.index(\"first item\")
        nxt = rows[y + 1] if y + 1 < len(rows) else \"\"
        if nxt.strip() and not nxt.startswith(\" \" * start):
            sys.exit(\"the continuation of a wrapped list item is not indented under it: %r\" % nxt)
        sys.exit(0)
sys.exit(\"the list item is not on screen\")
"
  '

# --- The same answer, after a quit and a relaunch, renders identically. -----------------------
#
# This is where the durable-chunk-boundary bug would show even if the live render happened to look
# right: a restore reads the ledger, and a ledger that stored WRAPPED text would come back wrapped
# for the old width.
# The ANSWER's own rows, in the transcript's own columns. The rail beside it carries an
# about-line that is written when a turn ends and read back when the ledger is restored, and the
# viewport may sit one row differently after a relaunch — neither is what this bullet is about.
answer_block() {
  shell-use text | cut -c35- | sed 's/[[:space:]]*$//' | python3 -c '
import sys
rows = sys.stdin.read().split("\n")
start = next((i for i, r in enumerate(rows) if "The accumulated document" in r), None)
if start is None:
    sys.exit("the answer is not on screen")
end = next((i for i, r in enumerate(rows) if "turn ended" in r), len(rows))
print("\n".join(rows[start:end]))
'
}
export -f answer_block

before="$(answer_block)"
tui_quit
tui_start "$REPO_ROOT/scripts/tui/fixtures/markdown.patch.yml"
t the_same_answer_renders_identically_after_a_relaunch \
  bash -c '
    see "'"$SPLIT_WORD"'" --timeout 30000 || exit 1
    after="$(answer_block)"
    [ "$after" = "'"$before"'" ] || {
      echo "the restored render differs from the live one"
      diff <(printf "%s\n" "'"$before"'") <(printf "%s\n" "$after") | head -20
      exit 1
    }
  '

tui_quit
