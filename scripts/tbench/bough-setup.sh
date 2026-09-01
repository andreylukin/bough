#!/bin/bash
# Installs the already-copied linux bough binary and prepares $BOUGH_HOME inside the
# mounted /agent-logs dir, so the ledger and the request-recorder transcripts survive
# on the host after the run. The model rides the home's own user patch.
set -e
install -m 0755 /installed-agent/bough-bin /usr/local/bin/bough
export BOUGH_HOME=/agent-logs/bough-home
mkdir -p "$BOUGH_HOME"
cat > "$BOUGH_HOME/bough.patch.yml" <<PATCH
entries:
  model.policy:
    config:
      interactive: "${BOUGH_MODEL}"
      unattended: "${BOUGH_MODEL}"
      prices: {}
PATCH
# BOUGH_HOME must survive into the agent command's shell; the env file the harness
# sources runs before this script, so append to the profile the tmux shell reads.
echo 'export BOUGH_HOME=/agent-logs/bough-home' >> "$HOME/.bashrc"
if [ -d /installed-agent/skills ]; then
  mkdir -p "$BOUGH_HOME/skills"
  cp /installed-agent/skills/*.md "$BOUGH_HOME/skills/" 2>/dev/null || true
fi
bough --version
