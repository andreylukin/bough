#!/usr/bin/env bash
# One-time machine setup for bough's dedicated agent user (idempotent, macOS).
#
#   sudo scripts/agent-user.sh [username]
#
# Creates a standard (non-admin, hidden, no-GUI-login) user the bough server
# runs as post-cutover, a shared group linking you and the agent, and a scoped
# sudoers rule so your account can `sudo -u <agent>` without a password. It
# never touches your own files: per-repo access is granted later, explicitly,
# with `bough grant <dir>`.
set -euo pipefail

AGENT="${1:-bough}"
GROUP="bough-work"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "error: macOS only" >&2
  exit 1
fi
if [ "$(id -u)" -ne 0 ]; then
  echo "error: run with sudo: sudo $0 [username]" >&2
  exit 1
fi
CALLER="${SUDO_USER:-}"
if [ -z "$CALLER" ] || [ "$CALLER" = "root" ]; then
  echo "error: run via sudo from your own account (SUDO_USER must be set)" >&2
  exit 1
fi

# --- agent user -------------------------------------------------------------
if id -u "$AGENT" >/dev/null 2>&1; then
  echo "==> user $AGENT already exists"
else
  echo "==> creating standard user $AGENT"
  # Random throwaway password: the account is only ever entered via sudo -u,
  # never by typing a password. Not recorded anywhere.
  PW="$(head -c 32 /dev/urandom | base64)"
  sysadminctl -addUser "$AGENT" -fullName "bough agent" \
    -shell /bin/zsh -home "/Users/$AGENT" -password "$PW"
fi
AGENT_HOME="$(dscl . -read "/Users/$AGENT" NFSHomeDirectory | awk '{print $2}')"
# Hide from the login window / fast-user-switching menu.
dscl . -create "/Users/$AGENT" IsHidden 1
if [ ! -d "$AGENT_HOME" ]; then
  createhomedir -c -u "$AGENT" >/dev/null
fi

# --- shared group -----------------------------------------------------------
if dseditgroup -o read "$GROUP" >/dev/null 2>&1; then
  echo "==> group $GROUP already exists"
else
  echo "==> creating group $GROUP"
  dseditgroup -o create -r "bough shared work" "$GROUP"
fi
for u in "$CALLER" "$AGENT"; do
  dseditgroup -o checkmember -m "$u" "$GROUP" >/dev/null 2>&1 ||
    dseditgroup -o edit -a "$u" -t user "$GROUP"
done
echo "==> $GROUP members: $CALLER, $AGENT"

# Group members may traverse the agent's home (to read logs under ~agent/.bough);
# everyone else stays out.
chgrp "$GROUP" "$AGENT_HOME"
chmod 750 "$AGENT_HOME"

# --- sudoers: you -> agent, passwordless ------------------------------------
SUDOERS="/etc/sudoers.d/bough-agent-user"
RULE="$CALLER ALL=($AGENT) NOPASSWD: ALL"
if [ -f "$SUDOERS" ] && grep -qxF "$RULE" "$SUDOERS"; then
  echo "==> sudoers rule already installed"
else
  echo "==> installing $SUDOERS"
  TMP="$(mktemp)"
  {
    echo "# Installed by bough scripts/agent-user.sh — lets $CALLER act as the"
    echo "# agent user without a password (bough grant, bough logs, debugging)."
    echo "$RULE"
  } > "$TMP"
  visudo -cf "$TMP" >/dev/null
  install -m 440 -o root -g wheel "$TMP" "$SUDOERS"
  rm -f "$TMP"
fi

# --- root-escalation template (OFF by default) -------------------------------
# The agent has no route to root. If you ever want to allow specific system
# commands, uncomment and list them here — the default answer for system-level
# changes is: do it from your own shell.
ESCALATION="/etc/sudoers.d/bough-agent"
if [ ! -f "$ESCALATION" ]; then
  echo "==> installing commented escalation template $ESCALATION"
  TMP="$(mktemp)"
  cat > "$TMP" <<EOF
# bough agent root-command allowlist — EMPTY AND DISABLED BY DEFAULT.
# To allow the agent user a specific root command, uncomment and edit, e.g.:
# $AGENT ALL=(root) NOPASSWD: /usr/sbin/apachectl restart
EOF
  visudo -cf "$TMP" >/dev/null
  install -m 440 -o root -g wheel "$TMP" "$ESCALATION"
  rm -f "$TMP"
fi

echo
echo "done. Next steps (as $CALLER):"
echo "  1. bough setup --agent-user     # migrate ~/.bough state, install the LaunchDaemon"
echo "  2. bough grant <project-dir>    # per-repo access for the agent"
