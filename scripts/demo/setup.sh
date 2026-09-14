#!/bin/sh
# setup.sh DIR — a fresh demo sandbox for the README recordings:
#   DIR/home     a HOME with only your provider keys (no history, no config)
#   DIR/wordfreq the fixture repo, committed, with its failing tests
# Every recording starts from exactly this state.
set -eu
dir=${1:?usage: setup.sh DIR}
here=$(cd "$(dirname "$0")" && pwd)

mkdir -p "$dir/home/.bough"
if [ -f "$HOME/.bough/env" ]; then
	grep -E '^(ANTHROPIC|OPENROUTER|OPENAI|CEREBRAS)_API_KEY=' "$HOME/.bough/env" > "$dir/home/.bough/env" || true
fi
# One model for every recording, so they tell the same story.
# DEMO_PLUGIN / DEMO_MODEL pick another.
cat > "$dir/home/.bough/bough.yml" <<EOF
- id: llm
  plugin: ${DEMO_PLUGIN:-llm-openrouter}
  config:
    model: ${DEMO_MODEL:-openai/gpt-6-astra}
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
