#!/usr/bin/env python3
"""Orchestration metrics for bench trials — the instrument for iterating on how the
agent uses subagents and background tasks.

Final pass/fail hides orchestration quality (ClawArena-Team: leaderboard scores
cluster in a ~10pt band while orchestration behaviors diverge >10x), so this scores
the HOW, from the trajectory in bench/state/bough.db:

  - delegated   : did the trial spawn any subagent (kind=subagent, origin=trial)?
  - parallel    : did a program fan out (Promise.all / multiple agent()/spawn())?
  - bg-used     : did it use background jobs (bashBg/bashWait/auto-background)?
  - polled      : the anti-pattern — sleep/until poll loops in bash to "wait"
  - parent-ctx  : the parent session's context tokens (delegation should keep it lean)
  - wall / cost / pass : the usual, for the Pareto view

Usage: python3 bench/orch_metrics.py <since_ts_seconds> [task]
"""
import json
import sqlite3
import sys
from collections import defaultdict

DB = "/Users/andrey/repos/bough/bench/state/bough.db"
RESULTS = "/Users/andrey/repos/bough/bench/results/results.jsonl"

since = int(sys.argv[1]) if len(sys.argv) > 1 else 0
task_filter = sys.argv[2] if len(sys.argv) > 2 else None

db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
db.row_factory = sqlite3.Row

rows = [json.loads(l) for l in open(RESULTS)]
rows = [
    r for r in rows
    if r["harness"] == "bough" and r["ts"] >= since and r.get("session")
    and (task_filter is None or r["task"] == task_filter)
]


def programs(sid):
    out = []
    for r in db.execute(
        "SELECT parts FROM messages WHERE session_id=? AND role='supervisor'", (sid,)
    ):
        try:
            for p in json.loads(r["parts"]):
                if p.get("type") == "tool_call" and p.get("name") == "run_steps":
                    out.append(p.get("input", {}).get("code", "") or "")
        except Exception:
            pass
    return out


def signals(sid):
    code = "\n".join(programs(sid))
    subs = db.execute(
        "SELECT count(*) c FROM sessions WHERE origin_id=? AND kind='subagent'", (sid,)
    ).fetchone()["c"]
    deleg_calls = sum(code.count(f) for f in ("agent(", "spawn(", "join(", "adopt("))
    parallel = ("Promise.all" in code) or (code.count("agent(") + code.count("spawn(") > 1)
    bg = ("bashBg(" in code) or ("bashWait(" in code)
    # Auto-background note shows up in tool RESULTS, not the code:
    for r in db.execute(
        "SELECT parts FROM messages WHERE session_id=? AND role='supervisor'", (sid,)
    ):
        if "moved to background as" in (r["parts"] or ""):
            bg = True
    # Poll anti-pattern: a sleep/until loop in a bash command used to 'wait'.
    polled = ("until " in code and "sleep" in code) or ("while" in code and "sleep" in code)
    ctx = db.execute(
        "SELECT context_tokens FROM sessions WHERE id=?", (sid,)
    ).fetchone()
    return {
        "subagents": subs,
        "deleg_calls": deleg_calls,
        "parallel": parallel,
        "bg": bg,
        "polled": polled,
        "parent_ctx": (ctx["context_tokens"] if ctx else 0) or 0,
    }


agg = defaultdict(list)
print(f"{'task':22}{'pass':>5}{'sub':>4}{'par':>4}{'bg':>4}{'poll':>5}{'p-ctx':>8}{'wall':>7}{'$':>8}")
for r in sorted(rows, key=lambda r: r["task"]):
    s = signals(r["session"])
    agg[r["task"]].append((r, s))
    print(
        f"{r['task'][:22]:22}{'Y' if r['pass'] else 'n':>5}{s['subagents']:>4}"
        f"{'Y' if s['parallel'] else '-':>4}{'Y' if s['bg'] else '-':>4}"
        f"{'!' if s['polled'] else '-':>5}{s['parent_ctx']//1000:>7}k"
        f"{r['wall_ms']//1000:>6}s{(r.get('cost_usd') or 0):>8.4f}"
    )

n = len(rows)
if n:
    deleg = sum(1 for r in rows if signals(r["session"])["subagents"] > 0)
    par = sum(1 for r in rows if signals(r["session"])["parallel"])
    bg = sum(1 for r in rows if signals(r["session"])["bg"])
    poll = sum(1 for r in rows if signals(r["session"])["polled"])
    pas = sum(r["pass"] for r in rows)
    print(f"\n{n} trials: pass {pas}/{n} | delegated {deleg}/{n} | parallel {par}/{n} "
          f"| bg-used {bg}/{n} | polled(anti) {poll}/{n}")
