#!/usr/bin/env python3
"""Render the confirmed-fix table of `docs/ux-audit-2.md` from `target/ux2/verdicts.tsv`.

The re-audit's tables are written by the run, never by hand — a row here exists because a walk
produced a verdict and a screenshot, and the screenshot path in the row is the file the walk
captured. Residuals stay hand-written: a residual is a judgement about severity and owner, which
the harness cannot make.

Usage:  scripts/ux2/report.py [verdicts.tsv]
"""

import sys
from pathlib import Path

TITLES = {
    "M12-overlays": "M16/M12 — the first launch names the product and points at help",
    "B5-cwd": "B5 — the file lands in the launch cwd",
    "M9-gutter": "M9 — a gutter column separates the rail from the transcript",
    "M10-streaming": "M10/M19 — no chunk boundary or literal marker survives on screen",
    "B1-focus": "B1 — the composer keeps the keyboard",
    "B6-rowkeys": "B6 — a tool row is reachable from the keyboard",
    "B2-scroll": "B2 — the paging keys scroll the transcript",
    "B2-badge": "B2 — an anchored viewport shows `N new`",
    "B2-end": "B2 — End returns to the latest row",
    "B3-slash": "B3 — a missed command keeps the sentence",
    "B4-paste": "B4 — a multi-line paste is one draft",
    "M11-search": "M11 — search shows snippets and a count, not ledger JSON",
    "M12-esc": "M12 — Esc dismisses the search overlay",
    "M14-stopkey": "M14 — the stop key is named while a turn runs",
    "B7-interrupt": "B7 — Esc interrupts and says so",
    "B7-exitarm": "B7 — an idle Ctrl+C asks before exiting",
    "M13-rail": "M13 — the rail collapses at 80 columns",
    "M24-status": "M24 — the status line names model, cwd and context",
    "B8-quit": "B8 — /quit says goodbye and restores the terminal",
    "M28-restore": "M28 — a relaunch restores the conversation",
    "draft-cleared": "(probe) three Ctrl+U clear a three-line pasted draft",
}


def main() -> int:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else "target/ux2/verdicts.tsv")
    rows = [line.rstrip("\n").split("\t") for line in path.read_text().splitlines() if line.strip()]
    print("| # | Finding | Persona | Verdict | Screenshot |")
    print("|---|---------|---------|---------|------------|")
    for persona, finding, sev, verdict, shot, _note in rows:
        mark = "fixed" if verdict == "fixed" else "**not fixed**"
        print(
            f"| {finding} | {TITLES.get(finding, finding)} ({sev}) | {persona} | {mark} "
            f"| [`{Path(shot).name}`]({shot}) |"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
