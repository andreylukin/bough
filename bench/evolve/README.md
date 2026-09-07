# Harness evolution

The base model is frozen (GPT-5.6 Luna via OpenRouter); the harness around it evolves.
Shape: AHE (arXiv 2604.25850) with HarnessCompass's generalization gate (2608.01918),
answering "Rethinking the Evaluation of Harness Evolution" (2607.12227) by keeping the
reported benchmark held out.

- `harness/prompt.md`, `harness/guidance.md`, `harness/knobs.json` — the components. They are
  passed to trials as config (`--ak prompt=`, `--ak guidance=`, `EFFORT`/`JSTOOL`/`MAXCOST`), so
  an iteration never rebuilds the binary (`bench/harbor/dist/bough-go-evolve`).
- `split.json` — Terminal-Bench 2.0 split (seed 4): 20 train, 15 val. Terminal-Bench 4.0 is
  the held-out test set and is never run during evolution.
- `evolve.py` — `baseline`, `iterate -n N`, `digest <job>`, `status`. One iteration: digest the
  best train run → the proposer (`claude -p`, Fable) edits ONE component with a prediction of
  which tasks flip → train k=2 → if train improved, val k=2 → keep only if val did not drop →
  commit or revert. `state.json` and `iterations/` are the log.

Cost: ~20 × 2 train + 15 × 2 val trials per kept iteration; at default effort ≈ $15, at
xhigh ≈ 5×.
