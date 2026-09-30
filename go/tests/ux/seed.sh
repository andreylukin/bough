#!/usr/bin/env bash
# seed: build a fixture HOME full of real-looking bough history for UX
# testing, by running real sessions against a cheap model.
#
#   go/tests/ux/seed.sh DIR         build DIR/.bough (DIR must not exist)
#
# Copying a person's own ~/.bough/history was the earlier recipe; it
# carries their work into every screenshot and agent context. Here serve
# runs on DIR as HOME with deepseek-v4.1-flash (cents for the whole set)
# and every session in sessions.tsv goes through the same API the web
# page uses, so the transcripts, metas, jobs, asks and project filings
# are the real formats, not hand-written JSONL.
#
# Needs OPENROUTER_API_KEY (from the environment or ~/.bough/env) and jq.
# BOUGH_BIN picks the binary; default: a fresh build of this checkout.
# The key is written into DIR/.bough/env so a persona's own turns in
# the fixture are live too: keep DIR out of the repo.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
[ $# -eq 1 ] || { echo "usage: $0 DIR" >&2; exit 2; }
fx="$1"
[ ! -e "$fx" ] || { echo "seed: $fx exists; pick a new dir" >&2; exit 2; }
key="${OPENROUTER_API_KEY:-$(sed -n 's/^OPENROUTER_API_KEY=//p' "$HOME/.bough/env" 2>/dev/null | tr -d '"'"'"'')}"
[ -n "$key" ] || { echo "seed: OPENROUTER_API_KEY unset and not in ~/.bough/env" >&2; exit 2; }
concurrency="${SEED_CONCURRENCY:-6}"

mkdir -p "$fx/.bough" "$fx/code"
fx="$(cd "$fx" && pwd)"
bin="${BOUGH_BIN:-$fx/.seed/bough}"
if [ -z "${BOUGH_BIN:-}" ]; then (cd "$root/go" && go build -o "$bin" ./cmd/bough); fi

token="$(openssl rand -hex 32)"
printf '%s\n' "$token" >"$fx/.bough/serve.token"
chmod 600 "$fx/.bough/serve.token"
printf 'OPENROUTER_API_KEY=%s\n' "$key" >"$fx/.bough/env"
chmod 600 "$fx/.bough/env"
cat >"$fx/.bough/bough.yml" <<'EOF'
- id: llm
  plugin: llm-openrouter
  config:
    model: deepseek/deepseek-v4.1-flash
- id: llm-small
  plugin: llm-openrouter
  config:
    service: llm-small
    model: deepseek/deepseek-v4.1-flash
EOF

# The repos the sessions work in: small, committed, each a different
# stack so file trees, diffs and commands look like real projects.
for repo in "$here"/repos/*/; do
  name="$(basename "$repo")"
  cp -R "$repo" "$fx/code/$name"
  git -C "$fx/code/$name" init -q -b main
  git -C "$fx/code/$name" add -A
  git -C "$fx/code/$name" -c user.name=Sam -c user.email=sam@example.com commit -qm "initial import"
done
mkdir -p "$fx/code/scratch" # a plain folder: local sessions there are read-only

port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
url="http://127.0.0.1:$port"
log="$fx/.seed/serve.log"
mkdir -p "$fx/.seed"
# Provider keys in this shell must not leak past the fixture's env file.
(cd "$fx" && exec env -u ANTHROPIC_API_KEY -u OPENAI_API_KEY -u OPENROUTER_API_KEY -u CEREBRAS_API_KEY \
  HOME="$fx" "$bin" serve --run "127.0.0.1:$port") >"$log" 2>&1 &
serve=$!
trap 'kill $serve 2>/dev/null; wait $serve 2>/dev/null || true' EXIT
api() { # api METHOD PATH [JSON]
  curl -sf -X "$1" "$url$2" -H "Authorization: Bearer $token" -H "Origin: $url" \
    -H 'Content-Type: application/json' ${3:+--data "$3"}
}
for _ in $(seq 1 150); do api GET /api/health >/dev/null && break; sleep 0.2; done
api GET /api/health >/dev/null || { cat "$log" >&2; echo "seed: serve did not start" >&2; exit 1; }


# settle ID: wait until the session's turn is over (60 s per turn at most:
# a flash turn takes seconds; one that hangs is left as it is). "idle" is
# also what a session reads before its child has written the first
# input, so it only counts once it has held for 10 s; archiving on the
# early read left two sessions with nothing but their meta line.
settle() {
  local st idle=0
  for _ in $(seq 1 120); do
    st="$(api GET "/api/sessions/$1" | jq -r '.session.status // .status')"
    case "$st" in
      running|queued|"") idle=0 ;;
      idle) idle=$((idle + 1)); [ "$idle" -lt 20 ] || { echo idle; return; } ;;
      *) echo "$st"; return ;;
    esac
    sleep 0.5
  done
  echo running
}

for p in "Acme web app" "Ledger tools"; do api POST /api/projects "$(jq -nc --arg n "$p" '{name:$n}')" >/dev/null; done

# One sessions.tsv line: repo, prompts (" || " between turns), then an
# after-step: none | archive | rename:<title> | project:<slug> |
# interrupt (stop the first turn two seconds in) | open (leave the last
# turn unfinished and the ask unanswered).
run() {
  local repo="$1" prompts="$2" after="$3" cwd id first rest turn
  cwd="$fx/code/$repo"
  first="${prompts%% || *}"
  id="$(api POST /api/sessions "$(jq -nc --arg c "$cwd" --arg p "$first" '{cwd:$c, prompt:$p}')" | jq -r '.id // .session.id')"
  [ -n "$id" ] && [ "$id" != null ] || { echo "seed: create failed for: $first" >&2; return; }
  if [ "$after" = interrupt ]; then sleep 2; api POST "/api/sessions/$id/interrupt" '{}' >/dev/null || true; echo "$id interrupted"; return; fi
  settle "$id" >/dev/null
  rest="${prompts#"$first"}"
  while [ -n "$rest" ]; do
    rest="${rest# || }"
    turn="${rest%% || *}"
    rest="${rest#"$turn"}"
    api POST "/api/sessions/$id/prompt" "$(jq -nc --arg t "$turn" --arg r "seed-$RANDOM$RANDOM" '{text:$t, rid:$r}')" >/dev/null
    settle "$id" >/dev/null
  done
  case "$after" in
    archive) api POST "/api/sessions/$id/archive" '{}' >/dev/null ;;
    rename:*) api POST "/api/sessions/$id/rename" "$(jq -nc --arg n "${after#rename:}" '{title:$n, name:$n}')" >/dev/null ;;
    project:*) api POST "/api/sessions/$id/project" "$(jq -nc --arg p "${after#project:}" '{project:$p}')" >/dev/null ;;
  esac
  echo "$id $(settle "$id") $repo: ${first:0:60}"
}

# bash 3.2 (macOS) has no `wait -n`: count running jobs instead; serve
# is one of them.
busy() { [ "$(jobs -rp | wc -l)" -gt "$1" ]; }
while IFS=$'\t' read -r repo prompts after; do
  case "$repo" in ''|'#'*) continue ;; esac
  while busy "$concurrency"; do sleep 1; done
  run "$repo" "$prompts" "${after:-none}" &
done <"$here/sessions.tsv"
while busy 1; do sleep 1; done

n="$(api GET /api/sessions | jq '.sessions | length')"
kill $serve 2>/dev/null; wait $serve 2>/dev/null || true
python3 "$here/fixture.py" age "$fx"
echo "seed: $n sessions in $fx (serve log: $log)"
