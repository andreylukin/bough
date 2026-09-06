# bough's local memory: drawer, index, reader

`memoryd.py` is a small HTTP daemon that bough's `memory-tier` row talks to.

- **Drawer.** Every chunk the agent saw, verbatim, in SQLite (`~/.bough/memory/memory.db`),
  keyed by session and seq. Spilled outputs are read back in full. Nothing is deleted.
- **Index.** FTS5 (BM25) plus a static embedding (model2vec `potion-base-8M`) over the full
  text of every chunk, fused by reciprocal rank. Milliseconds, no model, all sessions.
- **Reader.** Granite 4.0 H-Tiny on mlx-lm reads the top hits and answers in prose. Every
  value the answer asserts must occur verbatim in one of those chunks or the answer is
  dropped, so `tools.recall` returns either a value with the chunk and line it came from,
  or "not in memory". Only tool outputs, background jobs and ledger records count as
  evidence: the conversation's own prompts, replies and code, and earlier recall outputs,
  are indexed but never read, so a question cannot verify itself.
- **Ledger.** After each turn the reader extracts decisions, facts, failures and files
  from that turn's chunks, verified the same way, and stores them as chunks of kind
  `ledger`, so cross-session questions find them first.

Measured on this machine (M5 Pro), 25 hand-written exact-value questions over a 128K-token
session: 12 correct, 11 wrong, 2 abstained, 2 s per answer. H-Small as the reader scores
the same and is five times slower. The wrong answers are grounded values picked from the
wrong line, never invented ones. A recurrent-state memory (Granite state fed the full
text) scored 8 of 25 on the same questions and was dropped; see the design notes.

Setup (Apple silicon):

    mkdir -p ~/.bough/memory && cp memoryd.py start.sh ~/.bough/memory/
    cd ~/.bough/memory && uv venv --python 3.12 venv && . venv/bin/activate && uv pip install mlx-lm model2vec numpy
    ./start.sh          # downloads the reader (~4 GB) and the embedding model on first run

Then on the memory-tier row in `~/.bough/bough.yml`:

    config:
      memory_url: http://127.0.0.1:8765

`BOUGH_MEMORY_READER` swaps the reader model; `BOUGH_MEMORY_DB` moves the database. The
server is single-threaded on purpose: MLX streams are thread-local.
