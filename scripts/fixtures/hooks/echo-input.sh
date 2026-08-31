#!/bin/sh
# Records the JSON the host wrote on stdin to $HOOK_RECORD, then returns one `hint` action.
# The whole hook protocol in eight lines: one JSON object in, one JSON object out.
input=$(cat)
[ -n "$HOOK_RECORD" ] && printf '%s\n' "$input" >> "$HOOK_RECORD"
printf '{"actions":[{"kind":"hint","agent":"sol","text":"a hook said so"}],"note":"ok"}\n'
