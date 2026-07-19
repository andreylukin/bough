#!/usr/bin/env python3
"""Aggregate bench results: per task×harness pass rate, wall clock, tokens, cost.

usage: report.py [results/results.jsonl]
"""
import json
import statistics
import sys
from collections import defaultdict
from pathlib import Path

path = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "results/results.jsonl")
rows = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
if not rows:
    sys.exit("no results")

groups = defaultdict(list)
for r in rows:
    groups[(r["task"], r["harness"])].append(r)

tasks = sorted({t for t, _ in groups})
harnesses = sorted({h for _, h in groups})


def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def fmt(v, spec):
    return format(v, spec) if v is not None else "-".rjust(int(spec[1:].split(".")[0].rstrip(",f")))


print(f"{'task':<22}{'harness':<14}{'n':>3}{'pass':>7}{'wall s':>9}{'out tok':>10}{'cost $':>10}")
print("-" * 75)
for task in tasks + ["ALL"]:
    for h in harnesses:
        g = [r for (t, hh), rs in groups.items() if hh == h and (task == "ALL" or t == task) for r in rs]
        if not g:
            continue
        rate = sum(r["pass"] for r in g) / len(g)
        wall = med([r["wall_ms"] for r in g])
        print(
            f"{task:<22}{h:<14}{len(g):>3}{rate:>7.0%}"
            f"{fmt(wall / 1000 if wall is not None else None, '>9.1f')}"
            f"{fmt(med([r.get('tokens_out') for r in g]), '>10,.0f')}"
            f"{fmt(med([r.get('cost_usd') for r in g]), '>10.4f')}"
        )

print("\ncost per solved task:")
for h in harnesses:
    g = [r for (_, hh), rs in groups.items() if hh == h for r in rs]
    solved = sum(r["pass"] for r in g)
    spend = sum(r.get("cost_usd") or 0 for r in g)
    rate = f"{solved}/{len(g)} solved, ${spend:.4f} total"
    print(f"  {h:<14}{'∞' if not solved else f'${spend / solved:.4f}'}   ({rate})")

failures = [r for r in rows if not r["pass"]]
if failures:
    print("\nfailure taxonomy:")
    tally = defaultdict(int)
    for r in failures:
        tally[(r["harness"], r["task"], r.get("fail_reason") or "unclassified")] += 1
    for (h, t, reason), n in sorted(tally.items()):
        print(f"  {h:<14}{t:<22}{reason:<24}×{n}")
