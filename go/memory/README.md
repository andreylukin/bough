# bough's local memory model

`memoryd.py` holds one Granite 4.0 hybrid (Mamba-2 + attention) state per
session on mlx-lm. bough's `memory-tier` row feeds every history entry
into it as it lands (tool outputs in full, spilled ones included), saves
the state to disk at the end of each turn, and reloads it on resume. The
reasoner gets `tools.recall(question)` and a memory note on every new
request; the navigator's index lines and picks come from it too.

Setup (Apple silicon):

    mkdir -p ~/.bough/memory && cp memoryd.py start.sh ~/.bough/memory/
    cd ~/.bough/memory && uv venv --python 3.12 venv && . venv/bin/activate && uv pip install mlx-lm
    ./start.sh          # downloads mlx-community/granite-4.0-h-small-4bit (~18 GB) on first run

Then in `~/.bough/bough.yml`, on the memory-tier row:

    config:
      memory_url: http://127.0.0.1:8765

Measured on an M5 Pro with H-Small: ingest ~230 tokens/s, answers in
1–3 s, 5 of 6 exact on a 16K-token session of realistic tool output.
H-Tiny (`BOUGH_MEMORY_MODEL=mlx-community/granite-4.0-h-tiny-4bit`) is
five times faster to ingest and noticeably less reliable. Neither counts
well over hundreds of lines; ask for facts, not aggregates. State files
land in `~/.bough/memory/state/`, about 25 MB per 1K tokens for H-Small.

The server is single-threaded on purpose: MLX streams are thread-local.
