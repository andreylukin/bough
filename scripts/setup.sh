#!/usr/bin/env bash
# Fresh-machine bootstrap for bough. macOS and Linux — nothing here is confined;
# bough runs as you (spec §2). Installs toolchain deps with the platform's package
# manager, builds the binary, and links the `bough` server manager onto PATH.
# Safe to re-run.
#
# WHAT IS AND IS NOT PLATFORM-SPECIFIC. The dependency LIST is identical everywhere
# (git, cargo, node, rg, uv); only the command that installs it differs. So this file
# resolves one installer up front and the rest reads the same on both — which is
# also why a distro nobody here has tested is a missing installer line and not a
# port.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

OS="$(uname -s)"

# One installer, resolved once. `install_pkgs` takes the PACKAGE names for this
# platform; every caller below passes the same logical set.
if [ "$OS" = "Darwin" ]; then
  if ! command -v brew >/dev/null; then
    echo "error: Homebrew is required. Install it first:" >&2
    echo '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"' >&2
    exit 1
  fi
  install_pkgs() { brew install "$@"; }
  PKG_RG=ripgrep PKG_NODE=node PKG_UV=uv
elif command -v apt-get >/dev/null; then
  install_pkgs() { sudo apt-get install -y "$@"; }
  PKG_RG=ripgrep PKG_NODE=nodejs PKG_UV=
elif command -v dnf >/dev/null; then
  install_pkgs() { sudo dnf install -y "$@"; }
  PKG_RG=ripgrep PKG_NODE=nodejs PKG_UV=
elif command -v pacman >/dev/null; then
  install_pkgs() { sudo pacman -S --needed --noconfirm "$@"; }
  PKG_RG=ripgrep PKG_NODE=nodejs PKG_UV=uv
else
  # Named rather than guessed at: a wrong `sudo <installer> install` is worse than
  # a sentence telling you what to install.
  echo "error: no supported package manager found on $OS (looked for brew, apt-get, dnf, pacman)." >&2
  echo "  install these yourself, then re-run: git, node, ripgrep, uv" >&2
  exit 1
fi

if ! command -v git >/dev/null; then
  if [ "$OS" = "Darwin" ]; then
    echo "error: git not found — install the Xcode Command Line Tools: xcode-select --install" >&2
  else
    echo "error: git not found — install it with your package manager, then re-run." >&2
  fi
  exit 1
fi

# rg is what the prompt tells the model to search with; node is the fallback JS
# runtime for the code-mode sidecar. (cargo has its own block below — it is
# installed via rustup, not the package manager.) The cheap tier is a hosted model
# you pick in the model picker; the
# one piece of local inference is the OPTIONAL command-history embedding layer
# (sqlite-lembed + a ~25MB GGUF the server downloads lazily) — see the sqlite
# block below for the only setup it needs. There is no tunnel: the server binds
# loopback and has no auth layer (spec §17).
echo "==> checking packages"
need_bins=(node rg uv)
need_pkgs=("$PKG_NODE" "$PKG_RG" "$PKG_UV")
missing=()
for i in "${!need_bins[@]}"; do
  command -v "${need_bins[$i]}" >/dev/null && continue
  # An empty package name means this platform has no distro package for it —
  # `uv` on Debian/Fedora — so it is reported instead of silently skipped.
  if [ -z "${need_pkgs[$i]}" ]; then
    echo "note: ${need_bins[$i]} has no package here — install it with:" >&2
    echo "  curl -LsSf https://astral.sh/uv/install.sh | sh" >&2
  else
    missing+=("${need_pkgs[$i]}")
  fi
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "==> installing ${missing[*]}"
  install_pkgs "${missing[@]}"
else
  echo "==> all packages already installed"
fi

# Rust toolchain. bough is built from source — the server, the TUI and the CLIs are
# one `cargo build --release` — so this is the one hard dependency with no fallback.
# rustup is the supported install on both platforms; a distro `rustc` is fine too as
# long as it is recent enough to build the workspace.
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) ;;
  *) [ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin" && export PATH ;;
esac

if ! command -v cargo >/dev/null; then
  echo "==> installing the Rust toolchain via rustup"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
  case ":$PATH:" in
    *":$HOME/.cargo/bin:"*) ;;
    *) [ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin" && export PATH ;;
  esac
fi
if ! command -v cargo >/dev/null; then
  echo "error: cargo is still not on PATH — install Rust from https://rustup.rs and re-run." >&2
  exit 1
fi
echo "==> $(cargo --version) ok"

# A JS runtime for the code-mode sidecar (`bun` if present, else `node`). `node` is
# in the package list above, so this is already satisfied; bun is only ever an
# upgrade, never a requirement, and is left alone if the user has one.
if command -v bun >/dev/null; then
  echo "==> bun $(bun --version) — the code-mode sidecar will use it"
else
  echo "==> no bun; the code-mode sidecar will use node"
fi

# ast-grep: structural code search, taught in `prompt/searching.md` beside rg.
#
# It replaces the leta/language-server stack that used to back an `lsp.*` host
# function. That stack was macOS-only (a brew tap), needed a language server per
# language, and the measurement that retired it is blunt: one call in 184 programs on
# a machine where it WAS installed. One static binary that works identically on both
# platforms is worth more than a richer surface half the installs never had.
#
# NOT optional in the way leta was, and that is the point: the prompt names ast-grep
# unconditionally, so an install without it is an install where a documented tool is
# missing. It is still not fatal — the model is told to drop to rg — but it should be
# loud rather than a shrug.
if command -v ast-grep >/dev/null; then
  echo "==> ast-grep already installed"
elif [ "$OS" = "Darwin" ] && command -v brew >/dev/null; then
  echo "==> installing ast-grep (structural search) via brew"
  brew install ast-grep
elif command -v cargo >/dev/null; then
  echo "==> installing ast-grep (structural search) via cargo"
  cargo install ast-grep --locked
elif command -v npm >/dev/null; then
  echo "==> installing ast-grep (structural search) via npm"
  npm install -g @ast-grep/cli
else
  echo "warning: could not install ast-grep — no brew, cargo or npm."
  echo "  bough works without it (programs fall back to rg + view), but the prompt"
  echo "  documents it. Install it: https://ast-grep.github.io/guide/quick-start.html"
fi

# parallel-cli: the web search behind the `search()` host function.
#
# Unlike ast-grep, its absence is SILENT rather than loud, and deliberately so:
# `search()` is registered only when this binary is on PATH, and the prompt
# section that documents it is gated on the host function. An install without
# it is an install with no web search, not one advertising a tool it lacks.
#
# Auth is separate and interactive, so it is not attempted here: run
# `parallel-cli login`, or export PARALLEL_API_KEY.
if command -v parallel-cli >/dev/null; then
  echo "==> parallel-cli already installed"
elif command -v curl >/dev/null; then
  echo "==> installing parallel-cli (web search for search())"
  # The installer writes into ~/.local and assumes those directories exist; on
  # a fresh machine it exits non-zero without them.
  mkdir -p "$HOME/.local/share" "$HOME/.local/bin"
  if curl -fsSL https://parallel.ai/install.sh | bash; then
    case ":$PATH:" in
      *":$HOME/.local/bin:"*) ;;
      *) echo "  note: add $HOME/.local/bin to PATH so bough can find parallel-cli" ;;
    esac
    echo "  authenticate with: parallel-cli login   (or export PARALLEL_API_KEY)"
  else
    echo "warning: parallel-cli install failed — bough runs fine, without search()."
  fi
else
  echo "warning: no curl, skipping parallel-cli — bough runs fine, without search()."
fi

# SQLite needs nothing installed. rusqlite is built with `bundled`, so bough
# compiles its own extension-capable SQLite on both platforms — which is what
# retired the macOS Homebrew-libsqlite3 swap the TypeScript tree needed for the
# OPTIONAL history vector layer (sqlite-vec + sqlite-lembed, `history.similar()`).

echo "==> building bough (cargo build --release) — the first build takes a few minutes"
(cd "$ROOT" && cargo build --release)


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
# OPENAI_API_KEY=          # only for the openai: entries in the model picker
# OPENROUTER_API_KEY=      # only for the vendor/model entries in the model picker
# CLOUDFLARE_API_KEY=      # Workers AI: the @cf/ entries. Needs the account id too
# CLOUDFLARE_ACCOUNT_ID=   # the account the Workers AI endpoint is scoped to
# CLOUDFLARE_API_BASE=     # override the endpoint (an AI Gateway, or a test server)
# CEREBRAS_API_KEY=        # Cerebras Inference: the cerebras: entries
# CEREBRAS_API_BASE=       # override the endpoint (default https://api.cerebras.ai)
# BOUGH_PORT=4321
# BOUGH_CHEAP_MODEL=       # titles, ghost text, activity blurbs. Default: a cheap
#                          # frontier model. Also settable in the model picker.
EOF
  chmod 600 "$ENV_FILE"
fi

echo
echo "setup complete. Next:"
echo "  1. put your ANTHROPIC_API_KEY in $ENV_FILE"
echo "  2. bough start"
echo "(or run 'bough setup' — it walks both steps interactively)"
