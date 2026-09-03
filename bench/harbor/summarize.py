#!/usr/bin/env python3
"""Summarize Harbor job dirs: one row per trial (task, reward, cost, tokens, turns, code
blocks, errors, status, wall time), then per-task pass rates.

    python3 bench/harbor/summarize.py ~/.cache/bough-tbench/jobs/<job> [<job> ...]
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path


def _load(p: Path) -> dict:
    try:
        return json.loads(p.read_text())
    except Exception:
        return {}


def _reward(res: dict) -> float | None:
    v = res.get("verifier_result") or {}
    r = v.get("rewards") or {}
    if isinstance(r, dict) and r:
        return float(next(iter(r.values())))
    if isinstance(v.get("reward"), (int, float)):
        return float(v["reward"])
    return None


def _wall(res: dict) -> str:
    try:
        a, b = res["started_at"], res["finished_at"]
        s = (datetime.fromisoformat(b) - datetime.fromisoformat(a)).total_seconds()
        return f"{int(s) // 60}m{int(s) % 60:02d}s"
    except Exception:
        return "-"


def main(jobs: list[str]) -> None:
    rows = []
    for job in jobs:
        jd = Path(job).expanduser()
        for trial in sorted(p for p in jd.iterdir() if p.is_dir()):
            res = _load(trial / "result.json")
            if not res:
                continue
            task = trial.name.rsplit("__", 1)[0]
            ar = res.get("agent_result") or {}
            meta = ar.get("metadata") or {}
            exc = res.get("exception_info") or {}
            rows.append(
                {
                    "job": jd.name,
                    "task": task,
                    "reward": _reward(res),
                    "cost": ar.get("cost_usd"),
                    "in": ar.get("n_input_tokens"),
                    "out": ar.get("n_output_tokens"),
                    "turns": meta.get("turns"),
                    "code": meta.get("code_blocks"),
                    "errs": meta.get("errors"),
                    "status": meta.get("status") or (exc.get("exception_type") or "-"),
                    "wall": _wall(res),
                }
            )
    if not rows:
        print("no finished trials")
        return
    hdr = f"{'task':32s} {'rw':>3s} {'cost':>7s} {'in':>8s} {'out':>7s} {'turn':>4s} {'code':>4s} {'err':>3s} {'status':10s} {'wall':>8s}  job"
    print(hdr)
    for r in rows:
        rw = "-" if r["reward"] is None else ("1" if r["reward"] >= 1 else "0")
        cost = "-" if r["cost"] is None else f"${r['cost']:.3f}"
        print(
            f"{r['task']:32s} {rw:>3s} {cost:>7s} {str(r['in'] or '-'):>8s} {str(r['out'] or '-'):>7s} "
            f"{str(r['turns'] if r['turns'] is not None else '-'):>4s} {str(r['code'] if r['code'] is not None else '-'):>4s} "
            f"{str(r['errs'] if r['errs'] is not None else '-'):>3s} {str(r['status']):10s} {r['wall']:>8s}  {r['job']}"
        )
    print()
    by = defaultdict(list)
    for r in rows:
        if r["reward"] is not None:
            by[r["task"]].append(r["reward"] >= 1)
    for task, outcomes in sorted(by.items()):
        n = len(outcomes)
        k = sum(outcomes)
        print(f"{task:32s} {k}/{n}  ({100 * k / n:.0f}%)")


if __name__ == "__main__":
    main(sys.argv[1:] or ["~/.cache/bough-tbench/jobs"])
