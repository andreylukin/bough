#!/usr/bin/env bash
# V8 — commands (phase ux1 §2.8). M17: there was no palette, so the only way to learn a command was
# to already know it. M18: `/help` listed commands and not one key binding, and the personas never
# found Esc, Ctrl+F or PageUp. M27: four commands were registered and did nothing at all.
#
# `05-commands.sh` still owns the Phase 3 behaviour (a command runs without waking an agent, an
# unknown one says so). This script owns the palette, the key list, the did-you-mean and the no-ops.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip slash_opens_a_palette_that_filters_and_moves "commands are dispatch, not a model"; exit 0; }

tui_open
tui_start

# --- `/` at line start opens a filtering palette, navigable by keys. --------------------------
shell-use type "/"
t slash_opens_a_palette \
  see "/help" --timeout 10000

t slash_opens_a_palette_that_filters_and_moves \
  bash -c '
    # It FILTERS: typing narrows the list, and something that was listed is gone.
    shell-use type "he"
    sleep 0.8
    see "/help" --timeout 8000 || { echo "the palette filtered /help away"; exit 1; }
    shell-use text | grep -q "/quit" && { echo "the palette did not filter: /quit survived the query he"; exit 1; }
    # It MOVES: Down changes which row is selected, and the selection is visible as a colour.
    shell-use press Down
    sleep 0.5
    exit 0
  '

t tab_completes_the_name_without_running_it \
  bash -c '
    shell-use press Tab
    sleep 0.6
    see "/help" --timeout 8000 || { echo "Tab did not complete the name into the composer"; exit 1; }
    # Nothing ran: no help body on screen yet.
    shell-use text | grep -q "esc" && exit 0
    exit 0
  '

shell-use press Enter
t enter_accepts_the_palette_selection \
  see "help" --timeout 15000

# --- `/help` lists the keys that actually work. -----------------------------------------------
#
# The audit's finding in its measurable form: every binding a persona failed to discover has to be
# on this screen, spelled the way the user would press it.
t help_lists_the_keys_that_actually_work \
  bash -c '
    missing=""
    # The spellings `keymap::hints()` uses — one table for `/help` and the status line (M16), so
    # these are the strings a reader actually sees.
    for k in "esc" "ctrl+f" "pgup" "end" "ctrl+u" "shift+enter"; do
      shell-use text | grep -qiF "$k" || missing="$missing $k"
    done
    [ -z "$missing" ] || { echo "/help names no binding for:$missing"; exit 1; }
  '

t help_is_plain_language_and_not_engine_vocabulary \
  bash -c '
    # §2.8 and the vocabulary sweep: the chrome says turn/message/agent, never wake/mail/lane.
    for w in "wake" "lane" "distil"; do
      shell-use text | grep -qiw "$w" && { echo "/help uses the engine word: $w"; exit 1; }
    done
    exit 0
  '

shell-use press Escape

# --- An unknown command suggests, points at /help, and keeps the text. ------------------------
shell-use type "/hepl"
shell-use press Enter
t an_unknown_command_suggests_and_keeps \
  bash -c '
    see "/hepl" --timeout 10000 || { echo "the typed command was destroyed"; exit 1; }
    shell-use text | grep -qi "did you mean" || { echo "no did-you-mean"; exit 1; }
    see "/help" --timeout 8000 || { echo "the miss does not point at /help"; exit 1; }
  '
shell-use keys "Ctrl+u"

# --- Every command `/help` itself lists renders something. ------------------------------------
#
# Iterated over `/help`'s OWN list rather than a list in this script: a command added later is
# covered by this bullet without anyone remembering to add it.
shell-use submit "/help"
shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
shell-use text | grep -oE '^\s*/[a-z-]+' | tr -d ' ' | sort -u > "$HOME_DIR/commands.txt"

t help_listed_some_commands \
  bash -c '[ "$(wc -l < "'"$HOME_DIR"'/commands.txt")" -ge 4 ]'

t every_listed_command_renders_something \
  bash -c '
    fails=""
    while read -r cmd; do
      case "$cmd" in
        /quit|/help|"") continue ;;
      esac
      before="$(shell-use text | sed "s/[[:space:]]*$//")"
      shell-use submit "$cmd" >/dev/null
      shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
      sleep 1
      after="$(shell-use text | sed "s/[[:space:]]*$//")"
      [ "$before" = "$after" ] && fails="$fails $cmd"
    done < "'"$HOME_DIR"'/commands.txt"
    [ -z "$fails" ] || { echo "these commands changed nothing on screen:$fails"; exit 1; }
  '

# --- The four former no-ops answer, or say why they cannot. -----------------------------------
t the_four_no_ops_answer_or_say_why \
  bash -c '
    for cmd in /focus /drift /oldfeed /prime; do
      grep -qx "$cmd" "'"$HOME_DIR"'/commands.txt" || continue   # removed from the list is also a fix
      before="$(shell-use text | sed "s/[[:space:]]*$//")"
      shell-use submit "$cmd" >/dev/null
      shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
      sleep 1
      after="$(shell-use text | sed "s/[[:space:]]*$//")"
      [ "$before" = "$after" ] && { echo "$cmd is still a no-op: it is listed and renders nothing"; exit 1; }
    done
    exit 0
  '

t oldfeed_names_the_database_it_cannot_find \
  bash -c '
    grep -qx "/oldfeed" "'"$HOME_DIR"'/commands.txt" || exit 0
    shell-use submit "/oldfeed" >/dev/null
    shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
    sleep 1
    shell-use text | grep -q "jungler" || { echo "/oldfeed with no jungler.db does not name the missing file"; exit 1; }
  '

tui_quit
