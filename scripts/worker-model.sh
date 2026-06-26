#!/usr/bin/env bash
# Canonical worker model — sourced by install.sh and `bough update` so the file
# the installer fetches always matches what the engine loads.
#
# WORKER_MODEL_FILE *must* equal worker_runtime.gleam's `default_gguf`, otherwise
# the download lands under a name the worker never looks for (its actual default
# stays missing and the worker fails to start). Keep them in sync.

# Qwen2.5-Coder-3B-Instruct, Q4_K_M (~2 GB). Override the URL with BOUGH_MODEL_URL.
WORKER_MODEL_FILE="qwen2.5-coder-3b-instruct-q4_k_m.gguf"
WORKER_MODEL_URL="${BOUGH_MODEL_URL:-https://huggingface.co/Qwen/Qwen2.5-Coder-3B-Instruct-GGUF/resolve/main/${WORKER_MODEL_FILE}}"
WORKER_MODEL_DIR="${HOME}/.bough/models"

# Worker GGUFs we used to ship but no longer load — pruned on update to reclaim
# disk (e.g. machines that installed before the switch to Qwen).
WORKER_MODEL_STALE=("vibethinker-3b-q4_k_m.gguf")

# Download the worker model if it isn't already present. Resumes a partial file.
# Returns non-zero on failure so callers can decide whether to die or warn.
ensure_worker_model() {
  local path="$WORKER_MODEL_DIR/$WORKER_MODEL_FILE"
  if [ -f "$path" ]; then
    echo "==> worker model already present at $path"
    return 0
  fi
  echo "==> downloading worker model (~2 GB) to $path"
  mkdir -p "$WORKER_MODEL_DIR"
  # -C - resumes a partial download so a re-run after an interruption continues.
  curl -fSL -C - -o "$path" "$WORKER_MODEL_URL"
}

# Remove superseded worker GGUFs to reclaim disk. Safe to call repeatedly.
prune_stale_worker_models() {
  local f
  for f in "${WORKER_MODEL_STALE[@]}"; do
    if [ -f "$WORKER_MODEL_DIR/$f" ]; then
      echo "==> removing superseded worker model $f (reclaiming disk)"
      rm -f "$WORKER_MODEL_DIR/$f"
    fi
  done
}
