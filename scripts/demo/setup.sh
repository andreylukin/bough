#!/bin/sh
# setup.sh DIR — a fresh demo sandbox for the README recordings:
#   DIR/home     a HOME with only your provider keys (no history, no config)
#   DIR/wordfreq the fixture repo, committed, with its failing tests
# Every recording starts from exactly this state.
set -eu
dir=${1:?usage: setup.sh DIR}
here=$(cd "$(dirname "$0")" && pwd)

mkdir -p "$dir/home/.bough"
# Each key from the environment when it is set, else from ~/.bough/env.
# The file alone went stale once (a revoked key there, a live one in the
# shell), and every recording then failed with a 401.
: > "$dir/home/.bough/env"
for var in ANTHROPIC_API_KEY OPENROUTER_API_KEY OPENAI_API_KEY CEREBRAS_API_KEY; do
	val=$(printenv "$var" || true)
	if [ -n "$val" ]; then
		printf '%s=%s\n' "$var" "$val" >> "$dir/home/.bough/env"
	elif [ -f "$HOME/.bough/env" ]; then
		grep -E "^$var=" "$HOME/.bough/env" >> "$dir/home/.bough/env" || true
	fi
done
chmod 600 "$dir/home/.bough/env"
# One model for every recording, so they tell the same story. Opus 5,
# because gpt-6-astra wrote its conclusion ("tool calls returned no
# visible output") into the same reply as its programs in 3 of 3 runs,
# and the recording showed it verbatim. DEMO_PLUGIN / DEMO_MODEL pick another.
cat > "$dir/home/.bough/bough.yml" <<EOF
- id: llm
  plugin: ${DEMO_PLUGIN:-llm-openrouter}
  config:
    model: ${DEMO_MODEL:-anthropic/claude-opus-5}
# Short blocks open, long output folded: the program the model writes is
# the thing the recording is for, and "4 steps · ran 4 commands" hid it.
# DEMO_COLLAPSE=all|large|none overrides.
- id: ui
  plugin: ui
  config:
    collapse: ${DEMO_COLLAPSE:-large}
# The sandbox has no language server, so tools.lsp only ever answers
# "no language server for this directory" in red; leave it out.
- id: lsp
  plugin: lsp
  disabled: true
# The web row would contend for :7683 with the bough you already run;
# artifacts needs it, so it goes too.
- id: web
  plugin: web
  disabled: true
- id: artifacts
  plugin: artifacts
  disabled: true
EOF

mkdir -p "$dir/wordfreq"
cp "$here"/fixture/go.mod "$here"/fixture/*.go "$dir/wordfreq/"
git -C "$dir/wordfreq" init -q
git -C "$dir/wordfreq" add .
git -C "$dir/wordfreq" -c user.name=demo -c user.email=demo@example.com commit -qm "wordfreq: count words, top n"
echo "$dir"
