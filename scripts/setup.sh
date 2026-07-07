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

# node: builds web/. jj: workspace snapshots. llama.cpp: local worker (llama-server).
# cloudflared: `deno task tunnel` for phone access. (deno has its own block below —
# it needs a version floor, and may already be on PATH from the deno.land installer.)
echo "==> checking Homebrew packages"
brew_bins=(node jj llama-server cloudflared rg uv)
brew_pkgs=(node jj llama.cpp cloudflared ripgrep uv)
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

# Deno >= 2.9 via Homebrew. Claw Patrol's MITM proxy needs the server-side TLS
# handshake + SNI callback that landed in node:tls in 2.9; anything older silently
# breaks the egress gate. We install/upgrade through brew even when an older deno is
# already on PATH from the deno.land installer (brew upgrade would fail on that one).
deno_ok() {
  command -v deno >/dev/null || return 1
  local v
  v="$(deno --version | head -1 | awk '{print $2}')"
  [ "$(printf '%s\n' "2.9.0" "$v" | sort -V | head -1)" = "2.9.0" ]
}
if deno_ok; then
  echo "==> deno $(deno --version | head -1 | awk '{print $2}') ok"
elif brew list --formula deno >/dev/null 2>&1; then
  echo "==> upgrading deno via brew"
  brew upgrade deno
else
  echo "==> installing deno via brew"
  brew install deno
fi
if ! deno_ok; then
  echo "error: need deno >= 2.9 on PATH — have '$(command -v deno || echo none)'." >&2
  echo "  A non-brew deno (e.g. ~/.deno/bin) may be shadowing brew's; fix your PATH so" >&2
  echo "  $(brew --prefix)/bin comes first, or remove the old deno." >&2
  exit 1
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
# at runtime, so a failed download is a warning, not a setup failure. A machine
# configured for frontier-worker mode never loads the GGUF — skip the download.
. "$ROOT/scripts/worker-model.sh"
frontier_cfg="$(grep -E '^BOUGH_WORKER_FRONTIER=' "$HOME/.bough/env" 2>/dev/null | tail -1 | cut -d= -f2- || true)"
if [ -n "$frontier_cfg" ] && [ "$frontier_cfg" != "0" ]; then
  echo "==> BOUGH_WORKER_FRONTIER is set — skipping local worker model download"
elif ! ensure_worker_model; then
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
# BOUGH_WORKER_FRONTIER=1  # worker micro-tasks on claude-haiku-4-5 instead of the local Qwen (or set a model id)
EOF
  chmod 600 "$ENV_FILE"
fi

echo
echo "setup complete. Next:"
echo "  1. put your ANTHROPIC_API_KEY in $ENV_FILE"
echo "  2. bough start"
echo "(or run 'bough setup' — it walks both steps interactively)"
