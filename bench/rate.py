#!/usr/bin/env python3
"""Qualitative trajectory rater for bench trials — an LLM judge for HOW a problem
was solved, complementing the quantitative signals in orch_metrics.py.

Pass/fail says whether the final workspace was correct; orch_metrics says which
orchestration behaviors fired. Neither says whether the SOLVE was clean or a lucky
flail. This reads each trial's trajectory from bench/state/bough.db and asks a
strong judge model to score it on a fixed rubric, with the oracle verdict provided
as ground truth so a pass-by-luck and a clean solve are rated differently.

Rubric (1..5 each; the judge also flags the verification gap explicitly):
  approach      — was the strategy sound and targeted, or scattershot?
  efficiency    — directness: minimal wasted steps/tokens, few dead-ends?
  verification  — did it actually RUN the check and confirm, vs claim success?
  recovery      — on an error, did it diagnose and recover cleanly?

Usage:
  python3 bench/rate.py <since_ts_seconds> [task]     # rate a run
  RATE_MODEL=claude-sonnet-5 python3 bench/rate.py 0  # pick the judge model
Appends one JSON line per trial to results/ratings.jsonl and prints a table.
"""
import json
import os
import subprocess
import sqlite3
import sys
import time
from collections import defaultdict
from pathlib import Path

BENCH = Path(__file__).resolve().parent
DB = BENCH / "state" / "bough.db"
RESULTS = BENCH / "results" / "results.jsonl"
RATINGS = BENCH / "results" / "ratings.jsonl"
RUBRIC = ["approach", "efficiency", "verification", "recovery"]
JUDGE_MODEL = os.environ.get("RATE_MODEL")  # None => claude's strong account default

JUDGE_CONTRACT = """You are grading HOW a coding agent solved a benchmark task — the
trajectory, not just the outcome. You are given the task, the ORACLE VERDICT (ground
truth pass/fail on the final workspace), and a compact trace of what the agent did:
its text, the programs it ran, and the tool output it saw.

Score the trajectory 1..5 on each dimension (5 = excellent, 1 = poor):
- approach: was the strategy sound and targeted for THIS task, or scattershot/confused?
- efficiency: directness — minimal wasted steps and dead-ends, no needless re-reading/rework?
- verification: did it actually RUN the check/tests and confirm the result, or claim
  success without evidence? A trial that passed the oracle but never ran the check itself
  scores LOW here (it got lucky); a fail that verified honestly and ran out of road scores higher.
- recovery: when it hit an error or a failing check, did it diagnose the cause and fix it,
  or thrash / give up / paper over it?

Also set verification_gap=true iff the agent's final message CLAIMED success but the
oracle verdict is fail (or it never verified and passed by luck).

Reply with ONLY a JSON object, no prose, no fences:
{"approach":N,"efficiency":N,"verification":N,"recovery":N,
 "verification_gap":true|false,
 "rationale":"one sentence on the decisive trajectory quality",
 "notable":"the single most telling moment (a good move or a mistake)"}"""


def rows_since(since, task_filter):
    if not RESULTS.exists():
        return []
    out = []
    for line in RESULTS.read_text().splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if (r.get("harness") == "bough" and r.get("ts", 0) >= since and r.get("session")
                and (task_filter is None or r["task"] == task_filter)):
            out.append(r)
    return out


def transcript(db, sid, max_chars=6000):
    """Compact trace of a bench session: agent text, programs, tool output."""
    lines = []
    for role, parts in db.execute(
            "SELECT role, parts FROM messages WHERE session_id=? ORDER BY created_at", (sid,)):
        try:
            parsed = json.loads(parts)
        except (ValueError, TypeError):
            continue
        for p in parsed:
            t = p.get("type")
            if t == "text" and p.get("text"):
                lines.append(f"[{role}] {p['text'][:700]}")
            elif t == "tool_call":
                code = (p.get("input") or {}).get("code", "")
                if code:
                    lines.append(f"[program]\n{code[:900]}")
            elif t == "tool_result":
                err = " (ERROR)" if p.get("isError") else ""
                lines.append(f"[output{err}] {str(p.get('output', ''))[:500]}")
    text = "\n".join(lines)
    if len(text) > max_chars:  # keep head (framing) + tail (final belief)
        half = max_chars // 2
        text = text[:half] + "\n[... trimmed ...]\n" + text[-half:]
    return text


def extract_json(text):
    try:
        return json.loads(text)
    except ValueError:
        pass
    start = text.find("{")
    if start >= 0:
        depth = 0
        for i, c in enumerate(text[start:], start):
            depth += (c == "{") - (c == "}")
            if depth == 0 and c == "}":
                return json.loads(text[start:i + 1])
    raise ValueError("no JSON object in judge reply")


def rate(db, r):
    task_prompt = (BENCH / "tasks" / r["task"] / "prompt.md").read_text()
    verdict = "PASS" if r["pass"] else f"FAIL ({r.get('fail_reason') or 'unclassified'})"
    prompt = (f"{JUDGE_CONTRACT}\n\n# Task: {r['task']}\n\n{task_prompt}\n\n"
              f"# Oracle verdict (ground truth): {verdict}\n"
              f"# Trial stats: {r.get('turns')} turns, {r['wall_ms']//1000}s, "
              f"${r.get('cost_usd') or 0:.4f}\n\n# Trajectory\n\n{transcript(db, r['session'])}")
    cmd = ["claude", "-p", "--output-format", "json"]
    if JUDGE_MODEL:
        cmd += ["--model", JUDGE_MODEL]
    proc = subprocess.run(cmd, input=prompt, capture_output=True, text=True, timeout=600)
    if proc.returncode != 0:
        raise RuntimeError(f"judge exit {proc.returncode}: {proc.stderr[:300]}")
    return extract_json(json.loads(proc.stdout)["result"])


def main():
    since = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    task_filter = sys.argv[2] if len(sys.argv) > 2 else None
    rows = rows_since(since, task_filter)
    if not rows:
        print("no bough trials to rate (check the since-timestamp)", file=sys.stderr)
        return
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)

    print(f"{'task':22}{'pass':>5}{'appr':>5}{'effi':>5}{'veri':>5}{'reco':>5}"
          f"{'gap':>4}  rationale")
    agg = defaultdict(list)
    for r in sorted(rows, key=lambda r: (r["task"], r["trial"])):
        try:
            v = rate(db, r)
        except Exception as e:  # noqa: BLE001 — one bad judge reply shouldn't abort the run
            print(f"{r['task'][:22]:22}  rate failed: {e}", file=sys.stderr)
            continue
        for k in RUBRIC:
            agg[k].append(v.get(k) or 0)
        gap = "!" if v.get("verification_gap") else "-"
        print(f"{r['task'][:22]:22}{'Y' if r['pass'] else 'n':>5}"
              f"{v.get('approach', 0):>5}{v.get('efficiency', 0):>5}"
              f"{v.get('verification', 0):>5}{v.get('recovery', 0):>5}{gap:>4}  "
              f"{v.get('rationale', '')[:70]}")
        with open(RATINGS, "a") as fh:
            fh.write(json.dumps({"ts": int(time.time()), "task": r["task"],
                                 "trial": r["trial"], "session": r["session"],
                                 "pass": r["pass"], "model": r.get("model"),
                                 "variant": r.get("variant"), **v}) + "\n")

    if agg["approach"]:
        n = len(agg["approach"])
        means = {k: sum(agg[k]) / n for k in RUBRIC}
        print(f"\n{n} trajectories rated | " + " ".join(f"{k} {means[k]:.1f}" for k in RUBRIC))


if __name__ == "__main__":
    main()
