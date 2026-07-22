#!/usr/bin/env python3
"""Online ACE loop: mine real bough sessions for friction, turn recurring
patterns into candidate prompt edits, and queue them for the offline tuner.

This is the *online* half of the pipeline (the offline half is tune.py). It
implements the ACE (Agentic Context Engineering, arXiv 2510.04618) generator →
reflector → curator loop over natural execution feedback:

  generator  — the user's REAL daily-driver sessions (~/.bough/bough.db) are the
               rollouts; no labels needed.
  reflector  — claude -p (contract in online.md) reads compact transcripts of
               sessions that showed friction and proposes small prompt deltas.
  curator    — each delta is materialized as a variant dir under variants/,
               growth-capped and tagged source=online, then queued.

Crucially, online.py NEVER adopts anything. Candidates are only HYPOTHESES; the
haiku bench race in tune.py (run `tune.py --seed-online`, which nightly.sh does)
is the sole adoption gate. Real sessions run on a stronger model than the bench,
so a candidate that doesn't move the weak-model bench is dropped — the friction
is the idea source, the bench is the judge.

usage:
  bench/tune/online.py                 # mine last 14 days, emit candidates
  bench/tune/online.py --days 7 --max-sessions 10
  bench/tune/online.py --dry-run       # mine + print friction summary, no LLM
"""
import argparse
import json
import os
import re
import sqlite3
import subprocess
import time
from pathlib import Path

TUNE = Path(__file__).resolve().parent
VARIANTS = TUNE / "variants"
LEARNINGS = TUNE / "learnings.md"
PROMPT_DIR = TUNE.parent.parent / "src" / "supervisor" / "prompt"  # live/adopted
SECTION_FILES = ["system.md", "delegation.md", "delegation-nested.md",
                 "subagent.md", "ship-note.md"]
GROWTH_CAP = 1.05  # same dilution guard as the offline tuner
DEFAULT_DB = Path(os.environ.get("BOUGH_DB", Path.home() / ".bough" / "bough.db"))
QUEUE = VARIANTS / "_online-queue.json"


def log(msg):
    print(f"[online {time.strftime('%H:%M:%S')}] {msg}", flush=True)


# ---- friction mining ---------------------------------------------------------

def friction_sessions(db, since_ts, limit):
    """Recent root sessions that showed friction, worst first.

    Signals (cheap, SQL/transcript level): a failed check on a session that
    claimed to act, tool errors, and long user/assistant back-and-forth (rework).
    Subagents are excluded — we tune the top-level contract from what the user saw.
    """
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    rows = con.execute(
        """select id, title, model, kind, outcome_ok, outcome_check_passed,
                  output_tokens, created_at
           from sessions
           where created_at >= ? and kind = 'root'
           order by created_at desc""",
        (since_ts,)).fetchall()
    scored = []
    for s in rows:
        errs, user_turns, asst_turns = _turn_stats(con, s["id"])
        score = 0
        reasons = []
        if s["outcome_check_passed"] == 0 and s["outcome_ok"] is not None:
            score += 3
            reasons.append("check-failed")
        if errs >= 3:
            score += 2
            reasons.append(f"{errs}-tool-errors")
        if user_turns >= 4:
            score += 1
            reasons.append(f"{user_turns}-user-turns(rework)")
        if score:
            scored.append((score, s, reasons))
    con.close()
    scored.sort(key=lambda t: t[0], reverse=True)
    return scored[:limit]


def _turn_stats(con, session_id):
    errs = user_turns = asst_turns = 0
    for role, parts in con.execute(
            "select role, parts from messages where session_id=? order by created_at",
            (session_id,)):
        if role == "user":
            user_turns += 1
        elif role == "assistant":
            asst_turns += 1
        try:
            for p in json.loads(parts):
                if p.get("type") == "tool_result" and p.get("isError"):
                    errs += 1
        except (ValueError, AttributeError):
            continue
    return errs, user_turns, asst_turns


def transcript_excerpt(con, session_id, max_chars=2200):
    """Compact trace: agent text, the program it ran, tool output/errors."""
    lines = []
    for role, parts in con.execute(
            "select role, parts from messages where session_id=? order by created_at",
            (session_id,)):
        try:
            parsed = json.loads(parts)
        except ValueError:
            continue
        for p in parsed:
            t = p.get("type")
            if t == "text" and p.get("text"):
                lines.append(f"[{role}] {p['text'][:500]}")
            elif t == "tool_call":
                code = (p.get("input") or {}).get("code", "")
                if code:
                    lines.append(f"[program]\n{code[:600]}")
            elif t == "tool_result":
                err = " (ERROR)" if p.get("isError") else ""
                lines.append(f"[output{err}] {str(p.get('output', ''))[:400]}")
    text = "\n".join(lines)
    if len(text) > max_chars:
        half = max_chars // 2
        text = text[:half] + "\n[... trimmed ...]\n" + text[-half:]
    return text


# ---- current prompt + dedup context -----------------------------------------

def current_sections():
    out = {}
    for f in SECTION_FILES:
        p = PROMPT_DIR / f
        if p.exists():
            out[f] = p.read_text()
    return out


def current_chars():
    return sum(len(v) for v in current_sections().values())


def existing_hypotheses():
    """Hypotheses already tried (any variant) — so the reflector can dedup."""
    out = []
    for p in VARIANTS.iterdir():
        m = p / "meta.json"
        if p.is_dir() and m.exists():
            try:
                meta = json.loads(m.read_text())
                if meta.get("hypothesis"):
                    out.append(f"- {meta['name']}: {meta['hypothesis']}")
            except ValueError:
                continue
    return "\n".join(out[-60:])


def learnings_text(max_lines=60):
    if not LEARNINGS.exists():
        return ""
    return "\n".join(LEARNINGS.read_text().splitlines()[-max_lines:])


# ---- reflector ---------------------------------------------------------------

def extract_json(text):
    try:
        return json.loads(text)
    except ValueError:
        pass
    m = re.search(r"```(?:json)?\s*(\{.*\})\s*```", text, re.S)
    if m:
        return json.loads(m.group(1))
    start = text.find("{")
    if start >= 0:
        depth = 0
        for i, c in enumerate(text[start:], start):
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    return json.loads(text[start:i + 1])
    raise ValueError("no JSON object in reflector reply")


def reflect(frictions, con, model):
    parts = [(TUNE / "online.md").read_text()]
    parts.append("\n\n# Current prompt sections\n")
    for f, text in current_sections().items():
        parts.append(f"\n## FILE {f}\n\n{text}\n")
    parts.append(f"\n(current total: {current_chars()} chars — the per-candidate cap)\n")
    lt = learnings_text()
    if lt:
        parts.append("\n# Learnings (already refuted — do not re-propose)\n" + lt + "\n")
    eh = existing_hypotheses()
    if eh:
        parts.append("\n# Hypotheses already tried (dedup against these)\n" + eh + "\n")
    parts.append("\n# Friction sessions (worst first)\n")
    for score, s, reasons in frictions:
        parts.append(f"\n## session {s['id'][:8]} — {s['title'][:80]} "
                     f"[{', '.join(reasons)}] model={s['model']}\n")
        parts.append(transcript_excerpt(con, s["id"]))
    cmd = ["claude", "-p", "--output-format", "json"]
    if model:
        cmd += ["--model", model]
    r = subprocess.run(cmd, input="\n".join(parts), capture_output=True,
                       text=True, timeout=600)
    if r.returncode != 0:
        raise RuntimeError(f"reflector exit {r.returncode}: {r.stderr[:400]}")
    obj = extract_json(json.loads(r.stdout)["result"])
    return obj.get("candidates") or []


# ---- curator -----------------------------------------------------------------

def variant_chars(files):
    """Total chars of a full section set: candidate's changed files + current
    prompt's unchanged ones (what the bench will actually run)."""
    merged = dict(current_sections())
    merged.update(files)
    return sum(len(v) for v in merged.values())


def slug(name):
    return re.sub(r"[^a-z0-9-]", "-", (name or "online").lower()).strip("-") or "online"


def materialize(cand, campaign):
    files = cand.get("files") or {}
    bad = set(files) - set(SECTION_FILES)
    if bad or not files:
        return None, f"invalid files: {sorted(bad) or 'empty'}"
    name = f"online-{campaign}-{slug(cand.get('name'))}"
    d = VARIANTS / name
    n = 2
    while d.exists():
        d, n = VARIANTS / f"{name}-{n}", n + 1
    if variant_chars(files) > current_chars() * GROWTH_CAP:
        return None, "over growth cap"
    d.mkdir(parents=True)
    # Write changed files; fill the rest from the current prompt so the dir is a
    # complete, raceable section set (same convention as tune.py variants).
    full = dict(current_sections())
    full.update(files)
    for f in SECTION_FILES:
        if f in full:
            (d / f).write_text(full[f].rstrip() + "\n")
    meta = {
        "name": name, "source": "online", "mode": "online",
        "hypothesis": cand.get("hypothesis", ""),
        "prediction": cand.get("prediction", ""),
        "evidence": cand.get("evidence", ""),
        "parent": "baseline", "changed": sorted(files.keys()),
        "campaign": campaign, "ts": int(time.time()),
    }
    (d / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    return name, None


def enqueue(names):
    q = []
    if QUEUE.exists():
        try:
            q = json.loads(QUEUE.read_text())
        except ValueError:
            q = []
    q.extend(names)
    QUEUE.write_text(json.dumps(sorted(set(q)), indent=2) + "\n")


# ---- main --------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", type=Path, default=DEFAULT_DB)
    ap.add_argument("--days", type=float, default=14.0)
    ap.add_argument("--max-sessions", type=int, default=12)
    ap.add_argument("--model", default=None,
                    help="model for the claude -p reflector (default: claude's strong "
                         "account default). The reflector is the idea GENERATOR — keep it "
                         "strong; a weak model rewrites whole sections instead of surgical "
                         "deltas and bends harness facts. The weak-model bench is the judge, "
                         "not this.")
    ap.add_argument("--campaign", default=time.strftime("%Y-%m-%d"))
    ap.add_argument("--dry-run", action="store_true",
                    help="mine + print friction summary, no LLM, no candidates")
    args = ap.parse_args()

    if not args.db.exists():
        raise SystemExit(f"no bough db at {args.db}")
    since = int(time.time() - args.days * 86400)
    frictions = friction_sessions(args.db, since, args.max_sessions)
    log(f"{len(frictions)} friction session(s) in the last {args.days:g} days")
    for score, s, reasons in frictions:
        log(f"  [{score}] {s['id'][:8]} {s['title'][:60]!r} — {', '.join(reasons)}")
    if not frictions:
        log("no friction to learn from — nothing to do")
        return
    if args.dry_run:
        return

    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    con.row_factory = sqlite3.Row
    try:
        candidates = reflect(frictions, con, args.model)
    finally:
        con.close()
    log(f"reflector returned {len(candidates)} candidate(s)")

    queued = []
    for cand in candidates:
        name, err = materialize(cand, args.campaign)
        if err:
            log(f"  dropped {slug(cand.get('name'))}: {err}")
            continue
        queued.append(name)
        with open(LEARNINGS, "a") as fh:
            fh.write(f"- [{time.strftime('%Y-%m-%d')}] {name} (online): "
                     f"{cand.get('hypothesis', '')} -> QUEUED for bench race "
                     f"(evidence: {cand.get('evidence', 'n/a')})\n")
        log(f"  queued {name}")
    if queued:
        enqueue(queued)
        log(f"{len(queued)} candidate(s) queued; the next `tune.py --seed-online` "
            f"will race them. Queue: {QUEUE}")
    else:
        log("no candidates survived curation")


if __name__ == "__main__":
    main()
