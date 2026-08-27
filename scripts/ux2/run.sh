#!/usr/bin/env bash
# V11 — the UX re-audit (phase ux1 §3 V11). Three personas re-walk the top twelve findings of
# `docs/ux-audit-1.md` — B1..B8 and M9..M12 — against the RELEASE binary, each in its own empty
# `BOUGH_HOME` and its own empty scratch cwd, with live haiku for both tiers.
#
# Why this is not another `scripts/tui/*.sh`. The suite pins behaviour with the narrowest assertion
# that can fail; a re-audit has to answer a different question — would a person walking in cold now
# get through the thing that stopped them the first time. So each check here replays the AUDIT'S OWN
# repro line, captures the screen as an SVG, and records a verdict. The screenshots are the
# deliverable: `docs/ux-audit-2.md` cites one per confirmed fix.
#
# The gate: every blocker and every major verdict must be `fixed`. Anything else exits non-zero and
# belongs in the residuals table of `docs/ux-audit-2.md` with a severity and an owner crate.
#
# Usage:  scripts/ux2/run.sh [persona…]     (default: all three)
set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BOUGH_BIN="${BOUGH_BIN:-$REPO_ROOT/target/release/bough}"
SHOTS="$REPO_ROOT/docs/ux-audit-2-shots"
OUT="$REPO_ROOT/target/ux2"
VERDICTS="$OUT/verdicts.tsv"

command -v shell-use >/dev/null || { echo "ux2: shell-use is not on PATH"; exit 1; }
[ -x "$BOUGH_BIN" ] || { echo "ux2: no release binary at $BOUGH_BIN (run \`make release\`)"; exit 1; }

# Live haiku for BOTH tiers, exactly as the phase brief requires: the re-audit is about what a
# person sees, and half of what they see is a real model's real answer.
if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ -f "$HOME/.bough/env" ]; then
  set -a; . "$HOME/.bough/env"; set +a
fi
[ -n "${ANTHROPIC_API_KEY:-}" ] || { echo "ux2: no ANTHROPIC_API_KEY (the re-audit is live)"; exit 1; }

mkdir -p "$OUT"
: > "$VERDICTS"

PERSONAS="${*:-developer-critic andrey-owner keyboard-only-user}"

# ---------------------------------------------------------------------------
# the harness
# ---------------------------------------------------------------------------

PERSONA=""
STEP=0
FAILED=0

# `shot <slug>`: capture the screen as a full-colour SVG under this persona's directory. The
# numbering is the walk order, so the shots read as the walk.
shot() {
  STEP=$((STEP + 1))
  local n
  n="$(printf '%02d' "$STEP")"
  mkdir -p "$SHOTS/$PERSONA"
  shell-use screenshot "$SHOTS/$PERSONA/$n-$1.svg" >/dev/null 2>&1 || true
  echo "$SHOTS/$PERSONA/$n-$1.svg"
}

# `verdict <finding> <severity> <slug> <cmd…>`: run the check, capture a shot, record the row.
# A check that fails does NOT abort the walk — a re-audit that stops at the first residual tells
# you nothing about the other eleven.
verdict() {
  local finding="$1" sev="$2" slug="$3"; shift 3
  local v out img
  if out="$("$@" 2>&1)"; then v=fixed; else v=not-fixed; fi
  img="$(shot "$slug")"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$PERSONA" "$finding" "$sev" "$v" "${img#$REPO_ROOT/}" "$(printf '%s' "$out" | head -3 | tr '\n' ' ')" \
    >> "$VERDICTS"
  if [ "$v" = fixed ]; then
    echo "  ok   $finding ($sev)"
  else
    echo "  FAIL $finding ($sev): $(printf '%s' "$out" | head -1)"
    case "$sev" in blocker|major) FAILED=$((FAILED + 1)) ;; esac
  fi
}

see() { shell-use expect text --no-strict "$@"; }
absent() { shell-use expect text --not --no-strict "$@"; }

settle() { shell-use wait idle --timeout "${1:-30000}" >/dev/null 2>&1 || true; }

# `verdict` runs its check in a `bash -c` child; without this the helpers are simply not there and
# every check that uses one fails for the wrong reason.
export -f see absent

# ---------------------------------------------------------------------------
# one persona's walk
# ---------------------------------------------------------------------------

walk() {
  PERSONA="$1"
  STEP=0
  local home="$OUT/$PERSONA/home" cwd="$OUT/$PERSONA/cwd"
  rm -rf "$OUT/$PERSONA"
  mkdir -p "$home" "$cwd"

  # Whether this persona touches the mouse at all. keyboard-only-user does not, by construction —
  # B1, B6 and M26 are exactly the findings a mouseless walk could not even reach the first time.
  local mouse=1
  [ "$PERSONA" = keyboard-only-user ] && mouse=0

  export SHELL_USE_SESSION="bough-ux2-$PERSONA-$$"
  shell-use open --cols 120 --rows 36 --cwd "$cwd" >/dev/null
  shell-use submit "export BOUGH_HOME=$home" >/dev/null
  settle 5000
  shell-use submit "$BOUGH_BIN" >/dev/null
  settle 25000
  shot 00-first-launch

  echo "== $PERSONA =="

  # --- M12 first: the empty first launch has to say what this is. -----------------------------
  # (M16's half of the same screen. A persona reads the frame before they type anything.)
  verdict M12-overlays major first-launch-frame bash -c '
    see "bough" --timeout 15000 || { echo "no product name on an empty first launch"; exit 1; }
    see "help" --timeout 8000 || { echo "no help hint"; exit 1; }
  '

  # --- A real turn, so there is a transcript with a tool row in it. ---------------------------
  shell-use submit "Create a file called notes.txt in the current directory containing one line: hello from the re-audit. Then tell me, in two sentences, what you did." >/dev/null
  settle 120000
  shot 01-first-answer

  # --- B5 — the cwd. The disk is the only witness. --------------------------------------------
  verdict B5-cwd blocker cwd-landing bash -c '
    # `wait idle` settles the SCREEN; the tool write itself can land a beat later. Poll the disk
    # for a minute before calling it a miss — the claim is WHERE the file lands, not how fast.
    for _ in $(seq 60); do [ -f "'"$cwd"'/notes.txt" ] && break; sleep 1; done
    [ -f "'"$cwd"'/notes.txt" ] || { echo "notes.txt is not in the launch cwd"; exit 1; }
    cd "'"$REPO_ROOT"'" && [ -z "$(git status --porcelain -- notes.txt)" ] \
      || { echo "the file landed in the bough checkout instead"; exit 1; }
  '

  # --- M9 — the rail is separated from the transcript by a gutter. ----------------------------
  #
  # The audit asked for exactly this: "the strip owns its row and the transcript scrolls beneath
  # it; the rail is separated from content by at least one blank column, with hard clipping at the
  # boundary". So the check is the boundary column, not a guess at "two runs on a baseline" — a
  # rail row is ALLOWED to sit beside a transcript row, which is what a rail is; what is not
  # allowed is content butting straight up against it. The last two rows (status line, composer)
  # own their whole baseline by design, so they are excluded.
  verdict M9-gutter major gutter bash -c '
    shell-use text | python3 -c "
import sys
rows = sys.stdin.read().split(chr(10))
while rows and not rows[-1].strip():
    rows.pop()
body = rows[:-2] if len(rows) > 2 else rows
for y, row in enumerate(body):
    if len(row) > 34 and row[34] != chr(32):
        sys.exit(\"row %d has no gutter at column 34: %r\" % (y, row[28:44]))
"
  '

  # --- M10 — no chunk boundary survived as a line break. --------------------------------------
  verdict M10-streaming major streaming bash -c '
    shell-use text | python3 -c "
import sys
rows = [r.rstrip() for r in sys.stdin.read().split(chr(10))]
for y in range(len(rows) - 1):
    if rows[y].endswith(chr(45)):
        sys.exit(\"row %d ends in a bare hyphen: a word was split\" % y)
"
    shell-use text | grep -q "\*\*" && { echo "literal ** on screen: markdown was not parsed"; exit 1; }
    exit 0
  '

  # --- B1 / B6 — reaching a tool row and keeping the keyboard. --------------------------------
  if [ "$mouse" = 1 ]; then
    shell-use mouse click --on-text "write_file" >/dev/null 2>&1 || \
      shell-use mouse click --on-text "notes.txt" >/dev/null 2>&1 || true
    settle 5000
    shot 02-row-expanded
    verdict B1-focus blocker click-then-type bash -c '
      shell-use type "explain how git rebase works in detail"
      see "explain how git rebase works" --timeout 10000 \
        || { echo "the click killed the composer: nothing echoed"; exit 1; }
    '
    shell-use keys "Ctrl+u" >/dev/null
    verdict B6-rowkeys blocker keyboard-row bash -c '
      shell-use press Tab; sleep 0.8
      shell-use press Down; sleep 0.5
      shell-use press Enter; sleep 0.8
      shell-use text | grep -qi "notes.txt\|hello from the re-audit" \
        || { echo "Tab/Down/Enter reached no row"; exit 1; }
    '
  else
    verdict B1-focus blocker keyboard-focus bash -c '
      shell-use press Tab; sleep 0.8
      shell-use type "explain how git rebase works in detail"
      see "explain how git rebase works" --timeout 10000 \
        || { echo "a printable key did not snap the keyboard back to the composer"; exit 1; }
    '
    shell-use keys "Ctrl+u" >/dev/null
    verdict B6-rowkeys blocker keyboard-row bash -c '
      shell-use press Tab; sleep 0.8
      shell-use press Down; sleep 0.5
      shell-use press Space; sleep 0.8
      shell-use text | grep -qi "notes.txt\|hello from the re-audit" \
        || { echo "there is still no keyboard path to a tool row"; exit 1; }
    '
  fi
  shot 03-focus

  # --- B2 — scroll, follow, and a way back to live. -------------------------------------------
  shell-use submit "In roughly three hundred words, explain what a pseudo-terminal is and why a terminal multiplexer needs one." >/dev/null
  settle 120000
  top() { shell-use text | sed -n '4p' | cut -c30-; }
  before="$(top)"
  shell-use press PageUp >/dev/null; shell-use press PageUp >/dev/null
  sleep 1
  scrolled="$(top)"
  verdict B2-scroll blocker scroll bash -c '
    [ "'"$scrolled"'" != "'"$before"'" ] || { echo "PageUp from the composer did not scroll"; exit 1; }
  '
  # An answer that ends in a token nothing else on screen can contain: it is the only honest way to
  # ask "is the viewport showing the LATEST row", which is what B2 is about.
  shell-use submit "reply with exactly this and nothing else: ZQX-LATEST-MARKER" >/dev/null
  shot 04-anchored
  # The badge counts what arrived while the view is detached, so it can only appear once output
  # actually lands — poll for it rather than guessing at a sleep.
  verdict B2-badge blocker new-badge bash -c '
    for _ in $(seq 90); do
      shell-use text | grep -qE "[0-9]+ new" && exit 0
      sleep 1
    done
    echo "no unread affordance while scrolled up"; exit 1
  '
  settle 120000
  verdict B2-end blocker end-to-latest bash -c '
    # Columns 35 on: the TRANSCRIPT. The rail (columns 0-33) echoes the message the moment it is
    # sent, anchored or not, so a whole-screen grep would answer the wrong question.
    shell-use text | cut -c35- | grep -q "ZQX-LATEST-MARKER" \
      && { echo "the viewport followed while it was supposed to stay anchored"; exit 1; }
    shell-use press End
    sleep 2
    shell-use text | cut -c35- | grep -q "ZQX-LATEST-MARKER" \
      || { echo "End did not return to the latest row"; exit 1; }
  '
  shot 05-tail

  # --- B3 — a slash line that is not a command. -----------------------------------------------
  shell-use type "/tmp is where my files are" >/dev/null
  shell-use press Enter >/dev/null
  sleep 1.5
  shot 06-slash-miss
  verdict B3-slash blocker slash-miss bash -c '
    see "/tmp is where my files are" --timeout 10000 || { echo "the sentence was destroyed"; exit 1; }
    shell-use text | grep -qi "did you mean\|/help" || { echo "no did-you-mean and no pointer to /help"; exit 1; }
  '
  shell-use press Enter >/dev/null
  settle 90000
  shell-use keys "Ctrl+u" >/dev/null

  # --- B4 — a multi-line paste is ONE draft. --------------------------------------------------
  #
  # Sent the way a terminal actually delivers a paste: wrapped in `ESC[200~ … ESC[201~`. The shell
  # turns bracketed paste on at boot, so this is the path a real paste takes, and the newline-burst
  # heuristic is deliberately gated off while it is on (`run::on_key`). A caller that writes bare
  # newlines into a terminal that HAS announced bracketed paste is indistinguishable from a fast
  # typist, and still fires per-line sends — recorded as a residual in `docs/ux-audit-2.md`.
  shell-use write "$(printf '\033[200~alpha\nbeta\ngamma\033[201~')" >/dev/null
  sleep 1.5
  shot 07-paste
  verdict B4-paste blocker paste bash -c '
    see "alpha" --timeout 8000 || { echo "the first pasted line was swallowed"; exit 1; }
    see "gamma" --timeout 8000 || { echo "the last pasted line is missing"; exit 1; }
  '
  # Ctrl+U kills the LINE it is on (M20's fix), so a three-line draft takes three of them. Clearing
  # it here is not part of any finding — it is so that the checks below run against a composer in
  # the state a person would leave it in, not a wedged one.
  shell-use keys "Ctrl+u" >/dev/null
  shell-use keys "Ctrl+u" >/dev/null
  shell-use keys "Ctrl+u" >/dev/null
  sleep 0.5
  verdict draft-cleared minor draft-cleared bash -c '
    see "Type a message" --timeout 8000 \
      || { echo "three Ctrl+U left a three-line draft on screen"; exit 1; }
  '

  # --- M11 — search over rendered text. -------------------------------------------------------
  shell-use keys "Ctrl+f" >/dev/null
  sleep 1
  shell-use type "pseudo-terminal" >/dev/null
  sleep 2
  shot 08-search
  verdict M11-search major search bash -c '
    shell-use text | grep -q "request/header\|{\"" && { echo "raw ledger JSON in the results"; exit 1; }
    shell-use text | grep -qE "[0-9]+ of [0-9]+" || { echo "no hit count"; exit 1; }
    exit 0
  '
  shell-use press Escape >/dev/null
  sleep 0.8
  verdict M12-esc major esc-dismiss bash -c '
    absent "of " --timeout 8000 || true
    shell-use text | grep -qE "[0-9]+ of [0-9]+" && { echo "the search overlay survived Esc"; exit 1; }
    exit 0
  '

  # --- B7 / M14 — interrupt, and the key that does it being named. ----------------------------
  shell-use submit "Write eight paragraphs about the history of the terminal emulator." >/dev/null
  sleep 4
  shot 09-running
  verdict M14-stopkey major stop-key bash -c '
    shell-use text | grep -qi "esc" || { echo "nothing on screen names the interrupt key"; exit 1; }
  '
  shell-use press Escape >/dev/null
  sleep 3
  shot 10-interrupted
  verdict B7-interrupt blocker interrupt bash -c '
    shell-use text | grep -qi "interrupt" || { echo "Esc left no interrupted marker"; exit 1; }
  '
  shell-use keys "Ctrl+c" >/dev/null
  sleep 1
  verdict B7-exitarm blocker exit-arm bash -c '
    shell-use text | grep -qi "again to exit" || { echo "an idle Ctrl+C exits without asking"; exit 1; }
  '
  shell-use press Escape >/dev/null

  # --- M13 — the rail collapses. --------------------------------------------------------------
  shell-use resize 80 24 >/dev/null
  settle 10000
  sleep 1
  shot 11-80x24
  verdict M13-rail major rail-collapse bash -c '
    shell-use text | python3 -c "
import sys
rows = sys.stdin.read().split(chr(10))
worst = max((len(r.rstrip()) for r in rows), default=0)
if worst > 80:
    sys.exit(\"a row is %d columns wide in an 80-column terminal\" % worst)
"
  '
  shell-use resize 120 36 >/dev/null
  settle 10000

  # --- M24 — the status line says something. --------------------------------------------------
  shot 12-status
  verdict M24-status major status-line bash -c '
    txt="$(shell-use text)"
    for field in "haiku" "%"; do
      printf "%s" "$txt" | grep -q "$field" || { echo "the status line has no $field"; exit 1; }
    done
    printf "%s" "$txt" | grep -q "'"$(basename "$cwd")"'" || { echo "the status line does not name the cwd"; exit 1; }
    exit 0
  '

  # --- B8 — /quit says goodbye, restores the terminal, and is gone. ---------------------------
  local started
  started="$(date +%s)"
  shell-use submit "/quit" >/dev/null
  settle 15000
  shot 13-after-quit
  verdict B8-quit blocker quit bash -c '
    took=$(( $(date +%s) - '"$started"' ))
    [ "$took" -le 15 ] || { echo "/quit took ${took}s"; exit 1; }
    txt="$(shell-use text | sed "s/[[:space:]]*$//" | grep -v "^$" | tail -10)"
    [ -n "$txt" ] || { echo "the screen is blank after /quit"; exit 1; }
    shell-use type "echo terminal-is-back"
    see "echo terminal-is-back" --timeout 8000 || { echo "the shell does not echo: raw mode was never left"; exit 1; }
    shell-use press Enter
    see "terminal-is-back" --timeout 8000
  '

  # --- M28 — the relaunch restores every turn. ------------------------------------------------
  shell-use submit "$BOUGH_BIN" >/dev/null
  settle 25000
  shot 14-relaunch
  # A restored transcript is parked at its TAIL, so the walk cannot look for its first question on
  # the visible screen — it looks for the conversation, in the scrollback, and for a transcript
  # with real rows in it rather than the empty pane the audit reported.
  verdict M28-restore major restore bash -c '
    for _ in $(seq 25); do
      shell-use text --full | grep -qi "terminal emulator\|pseudo-terminal\|notes.txt\|/tmp is where" \
        && break
      sleep 1
    done
    shell-use text --full | grep -qi "terminal emulator\|pseudo-terminal\|notes.txt\|/tmp is where" \
      || { echo "the relaunch restored an empty transcript"; exit 1; }
    rows=$(shell-use text | sed -e "s/[[:space:]]*$//" | grep -c .)
    [ "${rows:-0}" -ge 6 ] || { echo "the restored transcript has only $rows non-blank rows"; exit 1; }
  '
  shell-use submit "/quit" >/dev/null
  settle 15000
  shell-use close >/dev/null 2>&1 || true
}

for p in $PERSONAS; do
  walk "$p"
done

echo
echo "== verdicts =="
column -t -s "$(printf '\t')" "$VERDICTS" 2>/dev/null || cat "$VERDICTS"
echo
echo "shots: $SHOTS"
echo "verdicts: $VERDICTS"

if [ "$FAILED" -gt 0 ]; then
  echo "ux2: $FAILED blocker/major verdicts are not \`fixed\` — they belong in the residuals table"
  exit 1
fi
echo "ux2: every blocker and major is confirmed fixed"
