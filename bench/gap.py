#!/usr/bin/env python3
"""Verification gap: bough trials whose transcript claims success but whose
oracle failed ("claimed-done-but-wrong"). This is the execution-alignment
number — it measures whether the harness forces the model to check its work
against the task's real contract, not its own restatement of it.

A trial counts as a *claim* when the final assistant text contains no explicit
failure admission (couldn't / unable / failed / blocked / ...). bough-only:
Claude Code trials don't keep transcripts in the bench state.

usage: gap.py [results/results.jsonl] [state/bough.db]
"""
import json
import re
import sqlite3
import sys
from pathlib import Path

HERE = Path(__file__).parent
results = Path(sys.argv[1] if len(sys.argv) > 1 else HERE / "results/results.jsonl")
db_path = Path(sys.argv[2] if len(sys.argv) > 2 else HERE / "state/bough.db")

ADMISSION = re.compile(
    r"couldn'?t|could not|unable to|failed to|cannot|can'?t\b|blocked|gave up|"
    r"not (?:able|possible)|unsuccessful",
    re.IGNORECASE,
)

db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)


def final_text(sid):
    rows = db.execute(
        "select parts from messages where session_id=? and role='supervisor' order by rowid",
        (sid,),
    ).fetchall()
    texts = [p["text"] for (parts,) in rows for p in json.loads(parts) if p.get("type") == "text"]
    return texts[-1] if texts else ""


trials = [json.loads(l) for l in results.read_text().splitlines() if l.strip()]
trials = [t for t in trials if t.get("harness") == "bough" and t.get("session")]
if not trials:
    sys.exit("no bough trials with session ids in " + str(results))

claims = gaps = 0
gap_rows = []
for t in trials:
    text = final_text(t["session"])
    claimed = bool(text) and not ADMISSION.search(text)
    claims += claimed
    if claimed and not t["pass"]:
        gaps += 1
        gap_rows.append(t)

print(f"bough trials: {len(trials)}   passed: {sum(t['pass'] for t in trials)}   claimed success: {claims}")
print(f"verification gap (claimed but oracle failed): {gaps}/{claims or 1}  ({gaps / (claims or 1):.0%})")
for t in gap_rows:
    print(f"  {t['task']} trial {t['trial']}  reason={t.get('fail_reason') or '?'}  session={t['session'][:8]}")
