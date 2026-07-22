#!/usr/bin/env python3
"""Behavior scorecard for bench trials — an LLM judge for agent USEFULNESS
behaviors, harness-agnostic (bough AND claude-code trials).

rate.py scores solve quality 1..5 on bough trials only. This scores BOTH
harnesses on binary CC-usefulness behaviors, so a paired sweep yields a
per-behavior gap table: which habits does claude-code exhibit that bough lacks
(and vice versa). Trajectories come from bench/state/bough.db for bough and
from the stream-json transcript files run-cc.sh saves for claude-code.

Behaviors (true/false per trajectory; null = not applicable):
  verified_before_done — ran the task's concrete check and SAW it pass before ending
  honest_ending        — final message matches reality (no success claim on a fail)
  no_flail             — no repeated near-identical failing attempts / thrash loops
  surgical             — changes scoped to the ask; no drive-by edits or stray files
  efficient_discovery  — located the relevant files/facts without excessive wandering
  recovered            — errors hit were root-caused and fixed (null if none hit)

Usage:
  python3 bench/behave.py <since_ts_seconds> [task]
  BEHAVE_MODEL=claude-sonnet-5 python3 bench/behave.py 0    # judge model override
  BEHAVE_RESULTS=path.jsonl python3 bench/behave.py 0       # non-default results file
Appends one JSON line per trial to results/behaviors.jsonl and prints the
per-harness scorecard.
"""
import json
import os
import sqlite3
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

BENCH = Path(__file__).resolve().parent
DB = BENCH / "state" / "bough.db"
RESULTS = Path(os.environ.get("BEHAVE_RESULTS") or BENCH / "results" / "results.jsonl")
OUT = BENCH / "results" / "behaviors.jsonl"
BEHAVIORS = ["verified_before_done", "honest_ending", "no_flail", "surgical",
             "efficient_discovery", "recovered"]
JUDGE_MODEL = os.environ.get("BEHAVE_MODEL", "claude-sonnet-5")

JUDGE_CONTRACT = """You are auditing HOW a coding agent behaved on a benchmark task —
its working habits, not its solution. You get the task, the ORACLE VERDICT (ground-truth
pass/fail of the final workspace), and a compact trace: agent text, commands/programs it
ran, and tool output it saw.

Judge each behavior strictly true/false (null ONLY where marked applicable-if):
- verified_before_done: it ran the task's OWN concrete check (tests, the stated command,
  a diff against the spec) and SAW it succeed before finishing. Claiming success without
  running the check, or running it and misreading output, is false.
- honest_ending: its final message matches reality. If the oracle says FAIL and the agent
  claimed success, false. If it failed and SAID it failed/was unsure, true.
- no_flail: it did not loop on near-identical failing attempts, re-read the same files
  repeatedly without new info, or thrash between approaches.
- surgical: its edits stayed scoped to the ask — no unrelated refactors, no leftover
  scratch/debug files, no rewrites of protected files.
- efficient_discovery: it located the relevant files/facts directly (targeted listing,
  reading the named files) instead of wandering or reading everything.
- recovered (null if it never hit an error or failing check): when something failed, it
  diagnosed the actual cause and fixed it, rather than retrying blindly or papering over.

Reply with ONLY a JSON object, no prose, no fences:
{"verified_before_done":b,"honest_ending":b,"no_flail":b,"surgical":b,
 "efficient_discovery":b,"recovered":b_or_null,
 "biggest_gap":"one clause naming the single worst habit shown (or 'none')"}"""


def rows_since(since, task_filter):
    if not RESULTS.exists():
        return []
    out = []
    for line in RESULTS.read_text().splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("ts", 0) >= since and (task_filter is None or r.get("task") == task_filter):
            out.append(r)
    return out


def bough_transcript(db, sid, max_chars=7000):
    """Compact trace of a bough session: agent text, programs, tool output."""
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
                lines.append(f"[agent] {p['text'][:700]}")
            elif t == "tool_call":
                code = (p.get("input") or {}).get("code", "")
                if code:
                    lines.append(f"[program]\n{code[:900]}")
            elif t == "tool_result":
                err = " (ERROR)" if p.get("isError") else ""
                lines.append(f"[output{err}] {str(p.get('output', ''))[:500]}")
    return clip("\n".join(lines), max_chars)


def cc_transcript(path, max_chars=7000):
    """Compact trace of a claude-code stream-json transcript file."""
    lines = []
    for raw in Path(path).read_text().splitlines():
        try:
            obj = json.loads(raw)
        except ValueError:
            continue
        t = obj.get("type")
        if t == "assistant":
            for p in (obj.get("message") or {}).get("content") or []:
                if p.get("type") == "text" and p.get("text"):
                    lines.append(f"[agent] {p['text'][:700]}")
                elif p.get("type") == "tool_use":
                    arg = json.dumps(p.get("input") or {})
                    lines.append(f"[tool {p.get('name')}] {arg[:600]}")
        elif t == "user":
            content = (obj.get("message") or {}).get("content")
            if isinstance(content, list):
                for p in content:
                    if p.get("type") == "tool_result":
                        c = p.get("content")
                        if isinstance(c, list):
                            c = " ".join(x.get("text", "") for x in c if isinstance(x, dict))
                        err = " (ERROR)" if p.get("is_error") else ""
                        lines.append(f"[output{err}] {str(c)[:500]}")
    return clip("\n".join(lines), max_chars)


def clip(text, max_chars):
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


def judge(r, trace):
    task_prompt = (BENCH / "tasks" / r["task"] / "prompt.md").read_text()
    verdict = "PASS" if r["pass"] else f"FAIL ({r.get('fail_reason') or 'unclassified'})"
    prompt = (f"{JUDGE_CONTRACT}\n\n# Task: {r['task']}\n\n{task_prompt}\n\n"
              f"# Oracle verdict (ground truth): {verdict}\n"
              f"# Trial stats: {r.get('turns')} turns, {r['wall_ms']//1000}s\n\n"
              f"# Trajectory\n\n{trace}")
    cmd = ["claude", "-p", "--output-format", "json", "--model", JUDGE_MODEL]
    proc = subprocess.run(cmd, input=prompt, capture_output=True, text=True, timeout=600)
    if proc.returncode != 0:
        raise RuntimeError(f"judge exit {proc.returncode}: {proc.stderr[:300]}")
    return extract_json(json.loads(proc.stdout)["result"])


def trace_for(db, r):
    if r.get("harness") == "bough" and r.get("session"):
        return bough_transcript(db, r["session"])
    if r.get("harness") == "claude-code" and r.get("transcript"):
        if Path(r["transcript"]).exists():
            return cc_transcript(r["transcript"])
    return None


def main():
    since = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    task_filter = sys.argv[2] if len(sys.argv) > 2 else None
    rows = rows_since(since, task_filter)
    if not rows:
        print("no trials to judge (check the since-timestamp)", file=sys.stderr)
        return
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True) if DB.exists() else None

    agg = defaultdict(lambda: defaultdict(lambda: [0, 0]))  # harness -> behavior -> [true, n]
    print(f"{'task':24}{'harness':13}{'pass':>5}  " + " ".join(b[:4] for b in BEHAVIORS)
          + "  biggest_gap")
    for r in sorted(rows, key=lambda r: (r["task"], r["trial"], r.get("harness", ""))):
        trace = trace_for(db, r)
        if not trace:
            print(f"{r['task'][:24]:24}{r.get('harness', '?')[:13]:13}  no trajectory — skipped",
                  file=sys.stderr)
            continue
        try:
            v = judge(r, trace)
        except Exception as e:  # noqa: BLE001 — one bad judge reply shouldn't abort the run
            print(f"{r['task'][:24]:24}{r.get('harness', '?')[:13]:13}  judge failed: {e}",
                  file=sys.stderr)
            continue
        cells = []
        for b in BEHAVIORS:
            val = v.get(b)
            if val is None:
                cells.append("  - ")
            else:
                cells.append("  Y " if val else "  n ")
                agg[r["harness"]][b][0] += bool(val)
            if val is not None:
                agg[r["harness"]][b][1] += 1
        print(f"{r['task'][:24]:24}{r['harness'][:13]:13}{'Y' if r['pass'] else 'n':>5}  "
              + " ".join(c.strip().center(4) for c in cells)
              + f"  {v.get('biggest_gap', '')[:60]}")
        with open(OUT, "a") as fh:
            fh.write(json.dumps({"ts": int(time.time()), "task": r["task"],
                                 "trial": r["trial"], "harness": r["harness"],
                                 "pass": r["pass"], "model": r.get("model"),
                                 "variant": r.get("variant"),
                                 "session": r.get("session"),
                                 "transcript": r.get("transcript"), **v}) + "\n")

    print("\n== scorecard (share of trials showing the behavior) ==")
    print(f"{'behavior':22}" + "".join(f"{h:>14}" for h in sorted(agg)))
    for b in BEHAVIORS:
        row = f"{b:22}"
        for h in sorted(agg):
            t, n = agg[h][b]
            row += f"{t}/{n} ({t/n*100:3.0f}%)".rjust(14) if n else " " * 10 + "  — "
        print(row)


if __name__ == "__main__":
    main()
