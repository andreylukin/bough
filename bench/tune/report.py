#!/usr/bin/env python3
"""Morning-after report for the prompt tuner: rank variants against baseline.

usage: bench/tune/report.py [results/tune-<campaign>.jsonl]   (default: newest campaign)
"""
import json
import sys
from collections import Counter
from pathlib import Path

TUNE = Path(__file__).resolve().parent
BENCH = TUNE.parent
VARIANTS = TUNE / "variants"


def results_file():
    if len(sys.argv) > 1:
        return Path(sys.argv[1])
    campaigns = sorted((BENCH / "results").glob("tune-*.jsonl"))
    return campaigns[-1] if campaigns else BENCH / "results" / "tune.jsonl"


def main():
    rows = []
    src = results_file()
    if src.exists():
        for line in src.read_text().splitlines():
            try:
                rows.append(json.loads(line))
            except ValueError:
                pass
    by_variant = {}
    for r in rows:
        by_variant.setdefault(r.get("variant") or "?", []).append(r)

    stats = {}
    for name, rs in by_variant.items():
        passes = sum(r["pass"] for r in rs)
        cost = sum(r.get("cost_usd") or 0 for r in rs)
        stats[name] = {
            "n": len(rs), "passes": passes,
            "cps": cost / passes if passes else None,
            "fails": Counter(r["fail_reason"] for r in rs if not r["pass"]),
        }
    if not stats:
        print(f"no tune results yet ({src} empty)")
        return
    print(f"campaign: {src.name}\n")

    base = stats.get("baseline")
    order = sorted(stats.items(), key=lambda kv: (-kv[1]["passes"] / max(kv[1]["n"], 1),
                                                  kv[1]["cps"] or 9e9))
    print(f"{'variant':<28} {'pass':>9} {'rate':>6} {'$/solved':>9} {'Δbase':>6}  hypothesis")
    for name, s in order:
        meta_p = VARIANTS / name / "meta.json"
        meta = json.loads(meta_p.read_text()) if meta_p.exists() else {}
        rate = s["passes"] / s["n"]
        delta = f"{(rate - base['passes'] / base['n']) * 100:+.0f}%" if base and name != "baseline" else ""
        cps = f"${s['cps']:.4f}" if s["cps"] else "-"
        hyp = (meta.get("hypothesis") or "")[:60]
        mark = "*" if meta.get("outcome") == "confirmed" else " "
        print(f"{mark}{name:<27} {s['passes']:>4}/{s['n']:<4} {rate * 100:>5.0f}% {cps:>9} {delta:>6}  {hyp}")
        if s["fails"]:
            top = ", ".join(f"{k}x{v}" for k, v in s["fails"].most_common(3))
            print(f"{'':<28} fails: {top}")
    print("\n* = promoted past its parent. Adopt a winner by porting its section files")
    print("back into src/supervisor/prompt.ts and re-verifying with bench/run.sh.")


if __name__ == "__main__":
    main()
