#!/usr/bin/env bash
# unreal-smoke.sh [anthropic|openai|openrouter ...] — one live pass of the
# engine-unreal loop per provider (go/docs/unreal-engine.md §15.3). Manual
# only, never part of `go test`: it spends real tokens (a few cents).
#
# Keys come from the environment only: ANTHROPIC_API_KEY, OPENAI_API_KEY,
# OPENROUTER_API_KEY. A missing key skips that provider with a line saying
# so. To use the ones in ~/.bough/env:  set -a; . ~/.bough/env; set +a
#
# Models (override with the variable):
#   SMOKE_ANTHROPIC_MODELS   "claude-sonnet-5 claude-opus-5"
#   SMOKE_OPENAI_MODELS      "gpt-5.6-sol"
#   SMOKE_OPENROUTER_MODELS  "openai/gpt-5.6-sol anthropic/claude-sonnet-5"
#
# Each (provider, model) runs seven sessions, headless --json, in a fresh
# temp HOME seeded only with a bough.yml overlay:
#   1 plain       two plain replies (Anthropic: cache_read grows on the 2nd)
#   2 parallel    two bash calls in one response
#   3 late        a call past the 1 s grace, then a question
#   4 cancel      SIGINT during a call (exit 130), then -r and a message
#   5 kill        kill -9 during a call, then -r and a message
#   6 think       /think high, then a reply
#   7 switch      /model to another provider mid-session (needs its key)
# and asserts: exit status, one done per prompt line, and no HTTP 400 in
# the engine trace. The scratch dir is kept when anything fails.
set -u
set +x # never trace: the environment holds keys

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/unreal-smoke.XXXXXX")
bin="$scratch/bough"
fails=0
runs=0

echo "unreal-smoke: scratch $scratch"
if ! (cd "$root/go" && go build -o "$bin" ./cmd/bough); then
	echo "unreal-smoke: build failed"
	exit 1
fi

# overlay PLUGIN MODEL: the rows this run changes over the embedded tree.
# session-title and activity are off so every request in the trace is
# the agent's own.
overlay() {
	cat <<EOF
- id: llm
  plugin: $1
  config:
    model: $2
- id: loop
  plugin: engine-unreal
  config:
    trace: true
- id: session-title
  plugin: session-title
  disabled: true
- id: activity
  plugin: activity
  disabled: true
EOF
}

# sandbox NAME PLUGIN MODEL: a fresh HOME and cwd for one session.
sandbox() {
	dir="$scratch/$1"
	mkdir -p "$dir/home/.bough" "$dir/cwd"
	overlay "$2" "$3" >"$dir/home/.bough/bough.yml"
}

# bough_in DIR ARGS...: the built binary under DIR's HOME and cwd.
bough_in() {
	local d=$1
	shift
	(cd "$d/cwd" && HOME="$d/home" BOUGH_WEB_ADDR=127.0.0.1:0 "$bin" --headless --json "$@")
}

session_id() {
	local f
	f=$(ls "$1/home/.bough/history/"*.jsonl 2>/dev/null | head -1)
	basename "${f%.jsonl}"
}

dones() { grep -c '"kind":"done"' "$1" 2>/dev/null || true; }

# check NAME COND MESSAGE: record one assertion.
check() {
	if [ "$2" = 1 ]; then
		return 0
	fi
	echo "  FAIL $1: $3"
	fails=$((fails + 1))
	return 1
}

# no400 DIR: the engine trace never saw a 400 (a render the provider
# rejected: the design's first risk).
no400() {
	if grep -qsE '"status": ?400' "$1/home/.bough/engine/trace/"*.jsonl; then
		return 1
	fi
	return 0
}

# expect NAME DIR OUT CODE WANTCODE WANTDONES
expect() {
	local ok=1
	[ "$4" = "$5" ] || { check "$1" 0 "exit $4, want $5"; ok=0; }
	local n
	n=$(dones "$3")
	[ "$n" = "$6" ] || { check "$1" 0 "$n done events, want $6"; ok=0; }
	no400 "$2" || { check "$1" 0 "HTTP 400 in the trace"; ok=0; }
	if [ $ok = 1 ]; then
		echo "  ok   $1"
	else
		tail -n 20 "$3" | sed 's/^/       /'
	fi
}

# wait_for FILE PATTERN SECONDS: false at once when the session died.
wait_for() {
	local i=0
	while [ $i -lt $(($3 * 10)) ]; do
		grep -qs "$2" "$1" && return 0
		kill -0 "$bg_pid" 2>/dev/null || return 1
		sleep 0.1
		i=$((i + 1))
	done
	return 1
}

# background DIR OUT LINE: start a session fed by a fifo and send LINE.
# Sets bg_pid; fd 3 holds the fifo open until the caller closes it.
background() {
	local fifo="$1/in.fifo"
	mkfifo "$fifo"
	(cd "$1/cwd" && HOME="$1/home" BOUGH_WEB_ADDR=127.0.0.1:0 exec "$bin" --headless --json <"$fifo" >"$2" 2>&1) &
	bg_pid=$!
	exec 3>"$fifo"
	printf '%s\n' "$3" >&3
}

# other PLUGIN: a second provider with a key, as "plugin model", for the
# switch scenario; empty when there is none.
other() {
	if [ "$1" != llm-openrouter ] && [ -n "${OPENROUTER_API_KEY:-}" ]; then
		echo "llm-openrouter openai/gpt-5.6-sol"
	elif [ "$1" != llm-anthropic ] && [ -n "${ANTHROPIC_API_KEY:-}" ]; then
		echo "llm-anthropic claude-sonnet-5"
	elif [ "$1" != llm-openai ] && [ -n "${OPENAI_API_KEY:-}" ]; then
		echo "llm-openai gpt-5.6-sol"
	fi
}

smoke() { # PLUGIN MODEL
	local plugin=$1 model=$2 tag d out code
	tag="$(echo "$plugin-$model" | tr '/:' '__')"
	echo "== $plugin $model"
	runs=$((runs + 1))

	d="$scratch/$tag-plain"
	sandbox "$tag-plain" "$plugin" "$model"
	out="$d/out.jsonl"
	printf 'Reply with exactly: pong\nReply with exactly: pong again\n' | bough_in "$d" >"$out" 2>&1
	code=$?
	expect plain "$d" "$out" $code 0 2
	if [ "$plugin" = llm-anthropic ]; then
		local cr
		cr=$(grep '"kind":"done"' "$d/home/.bough/history/"*.jsonl | sed -n 2p | grep -o '"cache_read":[0-9]*' | cut -d: -f2)
		check "plain cache" "$([ "${cr:-0}" -gt 0 ] && echo 1)" "second turn read ${cr:-0} cached tokens, want > 0"
	fi

	d="$scratch/$tag-parallel"
	sandbox "$tag-parallel" "$plugin" "$model"
	out="$d/out.jsonl"
	printf '%s\n' 'Make two bash calls in the same response, in parallel: `echo one` and `echo two`. Then reply with the two outputs.' | bough_in "$d" >"$out" 2>&1
	code=$?
	expect parallel "$d" "$out" $code 0 1
	local calls
	calls=$(grep -c '"kind":"call"' "$d/home/.bough/history/"*.jsonl 2>/dev/null)
	check "parallel calls" "$([ "${calls:-0}" -ge 2 ] && echo 1)" "$calls call rows, want >= 2"

	d="$scratch/$tag-late"
	sandbox "$tag-late" "$plugin" "$model"
	out="$d/out.jsonl"
	printf '%s\n' 'Run `sleep 5; echo late-done` with bash. While it runs, tell me what 2+2 is; then report its output.' | bough_in "$d" >"$out" 2>&1
	code=$?
	expect late "$d" "$out" $code 0 1
	check "late result" "$(grep -q late-done "$out" && echo 1)" "the late result never reached the reply"

	d="$scratch/$tag-cancel"
	sandbox "$tag-cancel" "$plugin" "$model"
	out="$d/out.jsonl"
	background "$d" "$out" 'Run `sleep 60` with bash, then say done.'
	if wait_for "$out" '"phase":"start"' 120; then
		kill -INT "$bg_pid"
		wait "$bg_pid"
		code=$?
		exec 3>&-
		expect cancel "$d" "$out" $code 130 1
		check "cancel order" "$(grep -q '"kind":"cancelled"' "$out" && echo 1)" "no cancelled event"
		out="$d/out2.jsonl"
		printf 'Reply with exactly: resumed\n' | bough_in "$d" -r "$(session_id "$d")" >"$out" 2>&1
		code=$?
		expect "cancel resume" "$d" "$out" $code 0 1
	else
		kill -KILL "$bg_pid" 2>/dev/null
		exec 3>&-
		check cancel 0 "no call started"
		tail -n 20 "$out" | sed 's/^/       /'
	fi

	d="$scratch/$tag-kill"
	sandbox "$tag-kill" "$plugin" "$model"
	out="$d/out.jsonl"
	background "$d" "$out" 'Run `sleep 60` with bash, then say done.'
	if wait_for "$out" '"phase":"start"' 120; then
		kill -KILL "$bg_pid"
		wait "$bg_pid" 2>/dev/null
		exec 3>&-
		out="$d/out2.jsonl"
		printf 'Reply with exactly: back\n' | bough_in "$d" -r "$(session_id "$d")" >"$out" 2>&1
		code=$?
		expect "kill resume" "$d" "$out" $code 0 1
	else
		kill -KILL "$bg_pid" 2>/dev/null
		exec 3>&-
		check kill 0 "no call started"
		tail -n 20 "$out" | sed 's/^/       /'
	fi

	d="$scratch/$tag-think"
	sandbox "$tag-think" "$plugin" "$model"
	out="$d/out.jsonl"
	printf '/think high\nReply with exactly: thought\n' | bough_in "$d" >"$out" 2>&1
	code=$?
	expect think "$d" "$out" $code 0 1

	local sw
	sw=$(other "$plugin")
	if [ -z "$sw" ]; then
		echo "  skip switch: no second provider key"
		return
	fi
	d="$scratch/$tag-switch"
	sandbox "$tag-switch" "$plugin" "$model"
	out="$d/out.jsonl"
	printf 'Reply with exactly: one\n/model %s\nReply with exactly: two\n' "$sw" | bough_in "$d" >"$out" 2>&1
	code=$?
	expect "switch to ${sw% *}" "$d" "$out" $code 0 2
}

want=" ${*:-anthropic openai openrouter} "
for p in anthropic openai openrouter; do
	case "$want" in *" $p "*) ;; *) continue ;; esac
	case $p in
	anthropic) key=${ANTHROPIC_API_KEY:-} models=${SMOKE_ANTHROPIC_MODELS:-claude-sonnet-5 claude-opus-5} ;;
	openai) key=${OPENAI_API_KEY:-} models=${SMOKE_OPENAI_MODELS:-gpt-5.6-sol} ;;
	openrouter) key=${OPENROUTER_API_KEY:-} models=${SMOKE_OPENROUTER_MODELS:-openai/gpt-5.6-sol anthropic/claude-sonnet-5} ;;
	esac
	if [ -z "$key" ]; then
		echo "== skip llm-$p: no $(echo "$p" | tr a-z A-Z)_API_KEY in the environment"
		continue
	fi
	for m in $models; do
		smoke "llm-$p" "$m"
	done
done
key=

if [ $runs = 0 ]; then
	echo "unreal-smoke: nothing ran (no keys)"
	rm -rf "$scratch"
	exit 0
fi
if [ $fails -gt 0 ]; then
	echo "unreal-smoke: $fails failed; kept $scratch"
	exit 1
fi
echo "unreal-smoke: all passed"
rm -rf "$scratch"
