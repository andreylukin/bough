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
#   2. installs gleam (Erlang/OTP), nono (sandbox), llama.cpp (worker model),
#      and rust (to build the monty code-mode sidecar)
#   3. clones bough to $BOUGH_HOME (default ~/repos/bough) if not already here
#   4. builds every package + the bough-monty sidecar
#   5. prints how to run it and what to set
#
# Env knobs:
#   BOUGH_HOME       where to clone if not run from a checkout (default ~/repos/bough)
#   BOUGH_REPO       git URL to clone (default https://github.com/andreylukin/bough.git)
#   BOUGH_NO_LLAMA=1 skip the (large) llama.cpp install; supervisor-only fixes still work
#   BOUGH_NO_MODEL=1 skip the worker model download (~1.9 GB)
#   BOUGH_MODEL_URL  override the GGUF download URL (default: VibeThinker-3B q4_k_m)
#   BOUGH_NO_MONTY=1 skip the rust toolchain + bough-monty sidecar build (no code-mode)

set -euo pipefail

BOUGH_HOME="${BOUGH_HOME:-$HOME/repos/bough}"
BOUGH_REPO="${BOUGH_REPO:-https://github.com/andreylukin/bough.git}"
# Default worker model — VibeThinker-3B (arXiv:2606.16140), a reasoning/coding
# SLM built on Qwen2.5-Coder-3B. Filename must match worker_runtime.gleam's
# default_gguf so bough finds it at ~/.bough/models/ without any env vars.
BOUGH_MODEL_FILE="vibethinker-3b-q4_k_m.gguf"
BOUGH_MODEL_URL="${BOUGH_MODEL_URL:-https://huggingface.co/oussaber/VibeThinker-3B-Q4_K_M-GGUF/resolve/main/${BOUGH_MODEL_FILE}}"

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
[ "${BOUGH_NO_MONTY:-0}" = "1" ] || deps+=(rust)
for d in "${deps[@]}"; do
  # `gleam`, `nono`, etc.; llama.cpp's binary is `llama-server`, rust's is `cargo`.
  bin="$d"
  [ "$d" = "llama.cpp" ] && bin="llama-server"
  [ "$d" = "rust" ] && bin="cargo"
  if have "$bin"; then
    info "$d already installed"
  else
    info "installing $d"
    brew install "$d"
  fi
done

# monty (the code-mode sidecar, built by `make build`) needs a recent stable
# Rust; refresh the toolchain here, before the build, when we manage it.
if [ "${BOUGH_NO_MONTY:-0}" != "1" ] && have rustup; then
  info "ensuring a recent stable Rust toolchain (monty needs >= 1.95)"
  rustup update stable >/dev/null 2>&1 || true
fi

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
# `make build` compiles every Gleam package and, unless BOUGH_NO_MONTY=1 (or
# cargo is missing), builds the bough-monty code-mode sidecar (Rust, embedding
# the monty interpreter — the BEAM can't host it in-process) and symlinks it
# into ~/.bough/bin. SPEC §5.2.
info "building all packages (+ the bough-monty code-mode sidecar)"
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
    info "downloading worker model (~1.9 GB) to $model_path"
    mkdir -p "$model_dir"
    # -C - resumes a partial download so a re-run after an interruption continues.
    curl -fSL -C - -o "$model_path" "$BOUGH_MODEL_URL" \
      || die "model download failed; re-run install.sh to resume, or set BOUGH_NO_MODEL=1 to skip"
  fi
fi

# 6. Put `bough` on PATH ---------------------------------------------------
# Symlink the launcher into Homebrew's bin (already on PATH from shellenv).
link_dir="$(brew --prefix)/bin"
ln -sf "$SRC/scripts/bough" "$link_dir/bough"
info "linked 'bough' -> $link_dir/bough"

# 7. Next steps ------------------------------------------------------------
cat <<EOF

$(info "bough is built at $SRC")

Set your API key (add to ~/.zshrc to make it permanent):
    export ANTHROPIC_API_KEY=sk-ant-...

Run it from anywhere (starts the server, then the TUI, cleans up on exit):
    bough

Or run the two processes by hand (two terminals):
    cd "$SRC" && make serve                       # terminal 1: the server (127.0.0.1:4096)
    cd "$SRC/packages/bough_tui" && gleam run      # terminal 2: the TUI client

The VibeThinker-3B worker is always on (local llama-server, no config needed).
The supervisor acts via the bough-monty code-mode sandbox (set BOUGH_MONTY_BIN
to override the binary). Optional knobs: BOUGH_MODEL, BOUGH_PROVIDER,
BOUGH_MAX_TURNS — see README.md.
EOF
