#!/bin/sh
# capture-web.sh DIR — record the web control room (bough serve) for the
# README: real sessions on the demo repo, run by a real model, then
# screenshots with agent-browser. Costs a little in tokens.
#
# Needs: bough on PATH, agent-browser, a provider key in ~/.bough/env.
# Writes assets/web-*.png in this checkout. Re-running on the same DIR
# reuses its sessions and only takes the screenshots again.
set -eu
dir=${1:?usage: capture-web.sh DIR}
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
addr=127.0.0.1:7699
api=http://$addr/api
repo="$dir/wordfreq"

# Only bough runs under the sandbox HOME; agent-browser keeps its
# socket under the real one (a long HOME overflows the socket path).
bough() { HOME="$dir/home" command bough "$@"; }

fresh=1
[ -n "$(ls "$dir/home/.bough/history" 2>/dev/null)" ] && fresh=0
[ $fresh = 1 ] && "$here/setup.sh" "$dir" >/dev/null
(cd "$repo" && bough serve "$addr" >/dev/null)
trap 'bough serve stop >/dev/null 2>&1 || true; agent-browser close >/dev/null 2>&1 || true' EXIT

json() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1]))' "$1"; }
create() { # prompt -> session id
	curl -sf -X POST "$api/sessions" -H 'content-type: application/json' \
		-d "{\"cwd\":$(json "$repo"),\"prompt\":$(json "$1")}" |
		python3 -c 'import json,sys; print(json.load(sys.stdin)["session"]["id"])'
}
status() { curl -sf "$api/sessions/$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session"]["status"])'; }
settle() { # wait until the session is no longer running (max ~5 min)
	i=0
	sleep 5
	while [ "$(status "$1")" = running ] && [ $i -lt 100 ]; do sleep 3; i=$((i + 1)); done
}
rename() { curl -sf -X POST "$api/sessions/$1/rename" -H 'content-type: application/json' -d "{\"title\":$(json "$2")}" >/dev/null; }

if [ $fresh = 1 ]; then
	# 1. The main story: failing tests, one fix, green.
	fix=$(create "go test fails in this repo. Fix TopN so ties are broken alphabetically and asking for more words than exist doesn't panic. Run the tests to prove it.")
	settle "$fix"
	rename "$fix" "Fix TopN ties and the out-of-range panic"

	# 2. A pasted screenshot, sent to the model as pixels.
	img=$(curl -sf -X POST "$api/attachments" -H 'content-type: image/png' --data-binary @"$root/assets/screenshot-palette.png" |
		python3 -c 'import json,sys; print(json.load(sys.stdin)["path"])')
	look=$(create "[Image #1: $img] This is bough's command palette. Suggest one concrete layout improvement, in two sentences.")
	settle "$look"
	rename "$look" "Review the palette layout from a screenshot"

	# 3. A session that stops to ask you something: the "needs you" state.
	ask=$(create "Before changing any code, use tools.ask to ask me whether Counts should treat 'Go' and 'go' as the same word (options: same, different). Then wait for my answer.")
	settle "$ask"
	rename "$ask" "Case-folding in Counts"
fi

agent-browser set viewport 1440 900 >/dev/null 2>&1 || true
agent-browser open "http://$addr/" >/dev/null
agent-browser wait 2000 >/dev/null
shoot() { # sidebar title prefix -> file [block label to expand]
	# Sidebar sessions are treeitems named "<status> <title>".
	ref=$(agent-browser snapshot 2>&1 | grep 'treeitem "' | grep -m1 " $1" | grep -o 'ref=e[0-9]*' | cut -d= -f2)
	[ -n "$ref" ] || { echo "no sidebar row for: $1" >&2; exit 1; }
	agent-browser click "@$ref" >/dev/null
	agent-browser wait 2500 >/dev/null
	if [ -n "${3:-}" ]; then # open the program the model wrote
		ref=$(agent-browser snapshot 2>&1 | grep -m1 -E "\"($3)" | grep -o 'ref=e[0-9]*' | cut -d= -f2)
		[ -n "$ref" ] && agent-browser click "@$ref" >/dev/null && agent-browser wait 800 >/dev/null
	fi
	agent-browser screenshot "$root/assets/$2" >/dev/null
	echo "assets/$2"
}
# The block's label depends on what the program did ("Program …", "Ran + program …").
shoot "Fix TopN" web-thread.png "Program|Ran"
shoot "Review the palette" web-image.png
# A restarted serve has no child left to answer, so the question only
# reads "waiting for you" on the run that asked it.
[ $fresh = 1 ] && shoot "Case-folding" web-ask.png
