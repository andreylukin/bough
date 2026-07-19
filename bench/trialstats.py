#!/usr/bin/env python3
"""Per-trial transcript stats for bough bench sessions — the measurement side
of the AHE loop. For each bough trial in results.jsonl (optionally filtered by
--since UNIX-SECONDS), digs into bench/state/bough.db and prints:

  rounds            run_steps calls in the turn
  err               program errors, with counts for known waste classes:
                      redecl  ('already been declared' / 'require is not defined')
                      lsp     (lsp verb failures)
  result_chars      total tool_result chars (billed-context proxy)
  checks            committed check commands: total/distinct (dup = re-run waste)
  check_kind        diff-vs-literal / run-only / none (exact-output task quality)

usage: trialstats.py [--since TS] [results/results.jsonl] [state/bough.db]
"""
import argparse
import json
import re
import sqlite3
from pathlib import Path

HERE = Path(__file__).parent
ap = argparse.ArgumentParser()
ap.add_argument("--since", type=int, default=0)
ap.add_argument("results", nargs="?", default=str(HERE / "results/results.jsonl"))
ap.add_argument("db", nargs="?", default=str(HERE / "state/bough.db"))
args = ap.parse_args()

REDECL = re.compile(r"already been declared|require is not defined")
LSPFAIL = re.compile(r"lsp|[Ll]anguage server|Symbol '.*' not found")
DIFFY = re.compile(r"diff\b|cmp\b|\[ \"?\$\(")

db = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)

rows = [json.loads(l) for l in open(args.results) if l.strip()]
rows = [r for r in rows if r.get("harness") == "bough" and r.get("session") and r["ts"] >= args.since]

print(f"{'task':<22}{'t':>3}{'pass':>5}{'rounds':>7}{'err':>4}{'redecl':>7}{'lsp':>4}"
      f"{'res_chars':>10}{'checks':>8}  check_kind")
for r in rows:
    parts = db.execute(
        "select parts from messages where session_id=? and role='supervisor' order by rowid",
        (r["session"],),
    ).fetchall()
    calls, results, checks = [], [], []
    for (p,) in parts:
        for blk in json.loads(p):
            if blk.get("type") == "tool_call" and blk.get("name") == "run_steps":
                calls.append(blk.get("input") or {})
                if (blk.get("input") or {}).get("check"):
                    checks.append(blk["input"]["check"])
            elif blk.get("type") == "tool_result":
                results.append(str(blk.get("content") or blk.get("output") or ""))
    errs = [t for t in results if "[program error]" in t or "error" in t.lower()[:80]]
    redecl = sum(1 for t in results if REDECL.search(t))
    lsp = sum(1 for t in results if LSPFAIL.search(t) and ("failed" in t or "not found" in t))
    kind = "none"
    if checks:
        kind = "diff-vs-literal" if any(DIFFY.search(c) for c in checks) else "run-only"
    print(f"{r['task']:<22}{r['trial']:>3}{r['pass']:>5}{len(calls):>7}{len(errs):>4}"
          f"{redecl:>7}{lsp:>4}{sum(len(t) for t in results):>10,}"
          f"{f'{len(checks)}/{len(set(checks))}':>8}  {kind}")
