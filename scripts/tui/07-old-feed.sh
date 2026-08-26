#!/usr/bin/env bash
# V7 (screen half) — §14's throwaway bridge, proven at the SURFACE and not only in a unit test: a
# planted jungler event becomes cited mail and shows up in the focus pane.
#
# `~/.jungler/jungler.db` may be absent on any machine (AGENTS.md), so this script PLANTS one in
# its own scratch home and points the row at it with a patch.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip jungler_mail_appears_in_the_focus_pane "the bridge is offline by construction"; exit 0; }

JUNGLER="$HOME_DIR/jungler.db"
sqlite3 "$JUNGLER" "
create table events (id integer primary key, at integer, kind text, subject text, body text, ref text, url text, lane text);
insert into events (id, at, kind, subject, body, ref, url, lane)
values (1, strftime('%s','now'), 'pr', 'JUNGLER-EVENT-ONE', 'a planted event body', 'gh:pr:1', 'https://example.invalid/1', 'main');
"

# The WHOLE config, not just the two fields this script cares about: a patch layer REPLACES an
# entry's `config` map rather than merging into it, so a partial one boots as
# "row `old-feed`: config could not be deserialized: missing field `bough_db`".
#
# `bough_db` is deliberately pointed at this script's own scratch home. The bundle's default is the
# developer's REAL `~/.bough/bough.db`, and a test that reads it is neither hermetic nor safe.
PATCH="$HOME_DIR/old-feed.patch.yml"
cat > "$PATCH" <<YML
entries:
  old-feed:
    # §17 Phase 6 retired this row ('disabled: true' in the bundle): the collectors replace it and
    # it stays for one week as the documented revert path. This script IS that revert path, so it
    # turns the row back on and proves the bridge still works when it is.
    #
    # No backticks anywhere in this heredoc: it is UNQUOTED so that it expands the paths above, and
    # a backticked phrase in a comment is still a command substitution to bash.
    disabled: false
    config:
      jungler_db: $JUNGLER
      bough_db: $HOME_DIR/bough.db
      state_db: $HOME_DIR/old-feed-state.db
      poll_ms: 200
      batch: 200
      deliver_to: sol
      priming_limit: 40
      tier1: true
YML

tui_open
tui_start "$PATCH"

t jungler_mail_appears_in_the_focus_pane \
  see "JUNGLER-EVENT-ONE" --timeout 30000

tui_quit
