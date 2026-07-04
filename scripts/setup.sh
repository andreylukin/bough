#!/usr/bin/env bash
# Fresh-machine bootstrap for bough (macOS only — the sandbox is Seatbelt-based).
# Installs toolchain deps via Homebrew, builds the web UI, fetches the worker
# model, and links the `bough` server manager onto PATH. Safe to re-run.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "error: bough's sandbox requires macOS (Seatbelt)." >&2
  exit 1
fi

if ! command -v brew >/dev/null; then
  echo "error: Homebrew is required. Install it first:" >&2
  echo '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"' >&2
  exit 1
fi

# git ships with the Xcode Command Line Tools; everything else comes from brew.
if ! command -v git >/dev/null; then
  echo "error: git not found — install the Xcode Command Line Tools: xcode-select --install" >&2
  exit 1
fi

# deno: runtime. node: builds web/. jj: workspace snapshots. llama.cpp: local
# worker (llama-server). cloudflared: `deno task tunnel` for phone access.
echo "==> checking Homebrew packages"
brew_bins=(deno node jj llama-server cloudflared)
brew_pkgs=(deno node jj llama.cpp cloudflared)
missing=()
for i in "${!brew_bins[@]}"; do
  command -v "${brew_bins[$i]}" >/dev/null || missing+=("${brew_pkgs[$i]}")
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "==> brew install ${missing[*]}"
  brew install "${missing[@]}"
else
  echo "==> all packages already installed"
fi

# Claw Patrol's MITM proxy needs Deno >= 2.9 (server-side TLS handshake + SNI
# callback in node:tls). An older brew deno silently breaks the egress gate.
deno_ver="$(deno --version | head -1 | awk '{print $2}')"
if [ "$(printf '%s\n' "2.9.0" "$deno_ver" | sort -V | head -1)" != "2.9.0" ]; then
  echo "==> deno $deno_ver is too old for the egress proxy — upgrading"
  brew upgrade deno
  deno_ver="$(deno --version | head -1 | awk '{print $2}')"
  if [ "$(printf '%s\n' "2.9.0" "$deno_ver" | sort -V | head -1)" != "2.9.0" ]; then
    echo "error: need deno >= 2.9, have $deno_ver" >&2
    exit 1
  fi
fi

# The plugin panel's "✎ Edit" button opens definitions in Zed. Optional but nice.
if ! command -v zed >/dev/null; then
  echo "warning: zed CLI not found — the Plugins panel's Edit button opens files in Zed." >&2
  echo "  Install Zed (brew install --cask zed), then run 'zed: install CLI' inside it." >&2
fi

echo "==> building web UI (server serves web/dist)"
(cd "$ROOT/web" && npm ci && npm run build)

echo "==> caching Deno dependencies + typecheck"
(cd "$ROOT" && deno install && deno task check)

# Worker model (~2 GB, resumes partial downloads). The local worker is optional
# at runtime, so a failed download is a warning, not a setup failure.
. "$ROOT/scripts/worker-model.sh"
if ! ensure_worker_model; then
  echo "warning: worker model download failed — re-run setup.sh to resume it" >&2
fi
prune_stale_worker_models

echo "==> linking bough CLI to ~/.local/bin/bough"
mkdir -p "$HOME/.local/bin"
ln -sf "$ROOT/scripts/bough" "$HOME/.local/bin/bough"
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo "warning: ~/.local/bin is not in PATH — add it to your shell profile:" >&2
     echo '  export PATH="$HOME/.local/bin:$PATH"' >&2 ;;
esac

# Env file the `bough` manager (and its launchd service) sources on start.
# Never commit this.
ENV_FILE="$HOME/.bough/env"
if [ ! -f "$ENV_FILE" ]; then
  echo "==> writing env template to $ENV_FILE"
  mkdir -p "$HOME/.bough"
  cat > "$ENV_FILE" <<'EOF'
# Environment for the bough server, sourced by `bough start` / the launchd service.
ANTHROPIC_API_KEY=
# OPENROUTER_API_KEY=      # only for the OpenRouter entries in the model picker
# BOUGH_PASSWORD=          # set to enable auth + LAN bind (needed for tunnel)
# BOUGH_PORT=4321
# BOUGH_HOST=
# BOUGH_CLAWPATROL=1       # opt-in native egress firewall (plugins, holds, rules)
EOF
  chmod 600 "$ENV_FILE"
fi

echo
echo "setup complete. Next:"
echo "  1. put your ANTHROPIC_API_KEY in $ENV_FILE"
echo "  2. bough start"
echo "(or run 'bough setup' — it walks both steps interactively)"
