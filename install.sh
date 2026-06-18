#!/usr/bin/env bash
# bough installer — gets you from a bare machine to a built bough.
#
# One-liner (no clone needed):
#   curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh | bash
#
# Or, from inside an existing checkout:
#   ./install.sh
#
# What it does, idempotently:
#   1. ensures Homebrew is present
#   2. installs gleam (Erlang/OTP), nono (sandbox), and llama.cpp (worker model)
#   3. clones bough to $BOUGH_HOME (default ~/repos/bough) if not already here
#   4. builds every package
#   5. prints how to run it and what to set
#
# Env knobs:
#   BOUGH_HOME       where to clone if not run from a checkout (default ~/repos/bough)
#   BOUGH_REPO       git URL to clone (default https://github.com/andreylukin/bough.git)
#   BOUGH_NO_LLAMA=1 skip the (large) llama.cpp install; supervisor-only fixes still work
#   BOUGH_NO_MODEL=1 skip the worker model download (~4.7 GB)
#   BOUGH_MODEL_URL  override the GGUF download URL (default: Qwen2.5-Coder-7B q4_k_m)

set -euo pipefail

BOUGH_HOME="${BOUGH_HOME:-$HOME/repos/bough}"
BOUGH_REPO="${BOUGH_REPO:-https://github.com/andreylukin/bough.git}"
# Default worker model — filename must match worker_runtime.gleam's default_gguf
# so bough finds it at ~/.bough/models/ without any env vars.
BOUGH_MODEL_FILE="qwen2.5-coder-7b-instruct-q4_k_m.gguf"
BOUGH_MODEL_URL="${BOUGH_MODEL_URL:-https://huggingface.co/Qwen/Qwen2.5-Coder-7B-Instruct-GGUF/resolve/main/${BOUGH_MODEL_FILE}}"

info() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m==>\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# 1. Homebrew --------------------------------------------------------------
if ! have brew; then
  info "Homebrew not found — installing it"
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  # Make brew available on this shell (Apple Silicon vs Intel paths).
  if [ -x /opt/homebrew/bin/brew ]; then eval "$(/opt/homebrew/bin/brew shellenv)"
  elif [ -x /usr/local/bin/brew ]; then eval "$(/usr/local/bin/brew shellenv)"; fi
fi
have brew || die "Homebrew install failed; install it manually from https://brew.sh"

# 2. System dependencies ---------------------------------------------------
deps=(gleam nono)
[ "${BOUGH_NO_LLAMA:-0}" = "1" ] || deps+=(llama.cpp)
for d in "${deps[@]}"; do
  # `gleam`, `nono`, etc.; llama.cpp's binary is `llama-server`.
  bin="$d"; [ "$d" = "llama.cpp" ] && bin="llama-server"
  if have "$bin"; then
    info "$d already installed"
  else
    info "installing $d"
    brew install "$d"
  fi
done

# 3. Source checkout -------------------------------------------------------
# If this script lives inside a checkout (Makefile next to it), build in place.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
if [ -f "$SCRIPT_DIR/Makefile" ] && [ -d "$SCRIPT_DIR/packages" ]; then
  SRC="$SCRIPT_DIR"
  info "building in place: $SRC"
else
  if [ ! -d "$BOUGH_HOME/.git" ]; then
    info "cloning bough into $BOUGH_HOME"
    mkdir -p "$(dirname "$BOUGH_HOME")"
    git clone "$BOUGH_REPO" "$BOUGH_HOME"
  else
    info "updating existing checkout at $BOUGH_HOME"
    git -C "$BOUGH_HOME" pull --ff-only || warn "could not fast-forward; using checkout as-is"
  fi
  SRC="$BOUGH_HOME"
fi

# 4. Build -----------------------------------------------------------------
info "building all packages"
make -C "$SRC" build

# 5. Worker model ----------------------------------------------------------
if [ "${BOUGH_NO_MODEL:-0}" = "1" ]; then
  info "skipping worker model download (BOUGH_NO_MODEL=1)"
else
  model_dir="$HOME/.bough/models"
  model_path="$model_dir/$BOUGH_MODEL_FILE"
  if [ -f "$model_path" ]; then
    info "worker model already present at $model_path"
  else
    info "downloading worker model (~4.7 GB) to $model_path"
    mkdir -p "$model_dir"
    # -C - resumes a partial download so a re-run after an interruption continues.
    curl -fSL -C - -o "$model_path" "$BOUGH_MODEL_URL" \
      || die "model download failed; re-run install.sh to resume, or set BOUGH_NO_MODEL=1 to skip"
  fi
fi

# 6. Next steps ------------------------------------------------------------
cat <<EOF

$(info "bough is built at $SRC")

Set your API key (add to ~/.zshrc to make it permanent):
    export ANTHROPIC_API_KEY=sk-ant-...

Run it (two terminals):
    cd "$SRC" && make serve                       # terminal 1: the server (127.0.0.1:4096)
    cd "$SRC/packages/bough_tui" && gleam run      # terminal 2: the TUI client

Or use the launcher (starts the server, then the TUI, and cleans up on exit):
    "$SRC/scripts/bough"

Optional knobs: BOUGH_MODEL, BOUGH_PROVIDER, BOUGH_WORKER, BOUGH_MAX_TURNS — see README.md.
EOF
