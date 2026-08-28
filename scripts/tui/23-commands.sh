#!/usr/bin/env bash
# V8 — commands (phase ux1 §2.8). M17: there was no palette, so the only way to learn a command was
# to already know it. M18: `/help` listed commands and not one key binding, and the personas never
# found Esc, Ctrl+F or PageUp. M27: four commands were registered and did nothing at all.
#
# `05-commands.sh` still owns the Phase 3 behaviour (a command runs without waking an agent, an
# unknown one says so). This script owns the palette, the key list, the did-you-mean and the no-ops.
source "$(dirname "$0")/lib.sh"

# The live half does not run this script. Every bullet it carries is named here, so the
# skip COUNT matches the count the replay half prints (a whole-script skip printing one
# `ok` line for ten assertions is the dishonesty `skip` exists to avoid).
[ -n "$BOUGH_LIVE" ] && {
  skip_all "commands are dispatch, not a model" \
  slash_opens_a_palette \
  slash_opens_a_palette_that_filters_and_moves \
  tab_completes_the_name_without_running_it \
  enter_accepts_the_palette_selection \
  question_mark_opens_the_help \
  help_lists_the_keys_that_actually_work \
  help_is_plain_language_and_not_engine_vocabulary \
  an_unknown_command_suggests_and_keeps \
  help_listed_some_commands \
  every_listed_command_renders_something \
  the_four_no_ops_answer_or_say_why \
  oldfeed_names_the_database_it_cannot_find
  exit 0
}

tui_open
tui_start

# --- `/` at line start opens a filtering palette, navigable by keys. --------------------------
shell-use type "/"
t slash_opens_a_palette \
  see "/help" --timeout 10000

# The query is `re`, which the NAME filter matches three ways (/reconsolidate, /reset, /resume):
# a one-item list — `he` matches only /help — has nowhere for Down to go, and the old spelling
# passed only because the palette overlay used to repaint the status row underneath it.
t slash_opens_a_palette_that_filters_and_moves \
  bash -c '
    shell-use type "re"
    sleep 0.8
    # It FILTERS: typing narrows the list, and something that was listed is gone.
    see "/reset" --timeout 8000 || { echo "the palette filtered /reset away"; exit 1; }
    shell-use text | grep -q "/quit" && { echo "the palette did not filter: /quit survived the query re"; exit 1; }
    # It MOVES: Down changes WHICH ROW is selected, and the selection is visible (`theme.sel_bg`).
    before="$(shell-use cells 0 0 200 60 --json)"
    shell-use press Down
    sleep 0.8
    after="$(shell-use cells 0 0 200 60 --json)"
    [ "$before" = "$after" ] && { echo "Down changed nothing: the palette selection does not move"; exit 1; }
    exit 0
  '
# Back to a one-item palette for the completion check below.
shell-use keys "Ctrl+u"
shell-use press Escape
shell-use type "/"
sleep 0.4
shell-use type "he"
sleep 0.8

# Tab must reach the PALETTE, not the pane ring. `action_for` used to map Tab to
# `Action::CycleFocus` unconditionally and `on_key` returned on that arm before the palette was
# ever consulted, so `PaletteAction::Complete` was dead code and Tab moved the keyboard off the
# composer with the palette still open. The old bullet asserted `see "/help"` — already on screen
# from the palette row — and then `grep -q "esc" && exit 0; exit 0`, which exits 0 either way.
#
# What a completion IS: the palette's selected name lands in the COMPOSER, with a trailing space,
# and nothing runs. The composer line is the last line of the frame, so the completed text is
# what the draft reads.
t tab_completes_the_name_without_running_it \
  bash -c '
    shell-use press Tab
    sleep 0.8
    # The composer now holds the completed command line, and the keyboard is still in it: typing
    # an argument goes into the draft rather than into a pane.
    shell-use type "xyzzy"
    sleep 0.5
    shell-use text | grep -q "/help xyzzy" || {
      echo "Tab did not complete into the composer (the draft does not read /help xyzzy)"
      shell-use text | tail -6
      exit 1
    }
    # And nothing RAN: the help body has no key table on screen.
    shell-use text | grep -q "shift+enter" && { echo "Tab RAN the command instead of completing it"; exit 1; }
    exit 0
  '
# Put the composer back the way the rest of the script expects it.
shell-use keys "Ctrl+u"
shell-use press Escape
shell-use type "/"
sleep 0.4

shell-use press Enter
t enter_accepts_the_palette_selection \
  see "help" --timeout 15000

# --- `/help` lists the keys that actually work. -----------------------------------------------
#
# The audit's finding in its measurable form: every binding a persona failed to discover has to be
# on this screen, spelled the way the user would press it.
# `?` on an empty draft opens the same help the palette does. Asserted BEFORE the list check, so
# the list below is over the help this key produced.
shell-use keys "Ctrl+u"
shell-use press Escape
shell-use type "?"
t question_mark_opens_the_help \
  bash -c '
    see "shift+enter" --timeout 10000 || { echo "? on an empty draft did not open /help"; exit 1; }
    shell-use text | grep -q "^?$" && { echo "? was typed into the composer instead of opening help"; exit 1; }
    exit 0
  '

t help_lists_the_keys_that_actually_work \
  bash -c '
    missing=""
    # The spellings `keymap::hints()` uses — one table for `/help` and the status line (M16), so
    # these are the strings a reader actually sees.
    # `?` is in the list because it is BOUND now (`Action::Help`). It was advertised by `hints()`
    # and by the status line while `action_for` had no arm for it at all — the one surface added
    # to tell the truth about the product was advertising a binding that did not exist.
    for k in "esc" "ctrl+f" "pgup" "end" "ctrl+u" "shift+enter" "?"; do
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
    # WAIT for it: the draft is on screen before Enter, so `see "/hepl"` proves nothing about
    # timing, and the notice lands a frame later.
    see "did you mean" --timeout 8000 || { echo "no did-you-mean"; exit 1; }
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
      # Start from a screen with NO notice up: the previous check left a persistent one (a
      # `usage:` line is an error notice, which waits for a key), and a command whose own
      # notice is the same text would then look like a no-op. Esc dismisses it.
      shell-use press Escape >/dev/null
      sleep 0.5
      before="$(shell-use text | sed "s/[[:space:]]*$//")"
      shell-use submit "$cmd" >/dev/null
      shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
      sleep 1
      after="$(shell-use text | sed "s/[[:space:]]*$//")"
      [ "$before" = "$after" ] && fails="$fails $cmd(screen-unchanged)"
      # The screen diff ALONE cannot fail: a dispatched command always raises a notice built from
      # `palette::echoed`, so its own name lands in the band whether or not it answered. The real
      # check is the marker `echoed` writes for an EMPTY answer (M27).
      shell-use text | grep -qF "no output" && fails="$fails $cmd(no-output)"
    done < "'"$HOME_DIR"'/commands.txt"
    [ -z "$fails" ] || { echo "these commands rendered nothing:$fails"; exit 1; }
  '

# --- The four former no-ops answer, or say why they cannot. -----------------------------------
t the_four_no_ops_answer_or_say_why \
  bash -c '
    for cmd in /focus /drift /oldfeed /prime; do
      grep -qx "$cmd" "'"$HOME_DIR"'/commands.txt" || continue   # removed from the list is also a fix
      # Start from a screen with NO notice up: the previous check left a persistent one (a
      # `usage:` line is an error notice, which waits for a key), and a command whose own
      # notice is the same text would then look like a no-op. Esc dismisses it.
      shell-use press Escape >/dev/null
      sleep 0.5
      before="$(shell-use text | sed "s/[[:space:]]*$//")"
      shell-use submit "$cmd" >/dev/null
      shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
      sleep 1
      after="$(shell-use text | sed "s/[[:space:]]*$//")"
      [ "$before" = "$after" ] && { echo "$cmd is still a no-op: it is listed and renders nothing"; exit 1; }
      # …and the same real check: the notice band names the command either way, so the falsifiable
      # half is that it did NOT answer with `palette::NO_OUTPUT` (M27).
      shell-use text | grep -qF "no output" && { echo "$cmd is listed and answers with nothing"; exit 1; }
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
