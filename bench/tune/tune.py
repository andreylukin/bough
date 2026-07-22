#!/usr/bin/env python3
"""Overnight prompt tuner: evolve bough's system prompt against the bench oracle.

Night-2 loop (upgrades over the v1 hill-climb, each mapped to a night-1 failure):
  1. REFLECT   — proposer sees failed-trial transcripts from the bench DB, not
                 just fail_reason tags (GEPA-style; v1 misdiagnosed mechanisms).
  2. RACE      — challengers screen at n=1 on all tasks; only survivors get the
                 full n=3 (TRIPLE/successive-halving; v1 spent full sweeps on
                 obvious losers).
  3. LEARN     — learnings.md persists verdicts across campaigns so refuted
                 directions stay dead (v1 history reset nightly).
  4. VARIANCE  — a task ledger pooled over all past results marks coin-flip
                 tasks; promotion weights stable tasks 2x noisy ones (v1 let
                 one noisy task crown a champion).
  5. PARETO    — per-task best variants form a frontier; every 3rd proposal
                 merges two frontier members instead of mutating the champion.
  Plus an end-of-night auto-confirmation: the final champion re-sweeps at n=6
  on its moved tasks vs baseline before the report may call it promoted.

usage:
  bench/tune/tune.py --hours 8 --trials 3          # overnight, all tasks
  bench/tune/tune.py --max-variants 1 --trials 1 --tasks bugfix-inventory  # smoke
  bench/tune/tune.py --sweep-only NAME             # re-score an existing variant

Morning after: bench/tune/report.py
"""
import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

TUNE = Path(__file__).resolve().parent
BENCH = TUNE.parent
VARIANTS = TUNE / "variants"
LEARNINGS = TUNE / "learnings.md"
BENCH_DB = BENCH / "state" / "bough.db"
# Repointed per campaign in main(): variants are only comparable when swept with
# the same task set and trial count, so each campaign gets its own results file
# and re-sweeps the baseline under its own conditions.
results_file = BENCH / "results" / "tune.jsonl"
LOGS = BENCH / "results" / "tune-logs"
SECTION_FILES = ["system.md", "delegation.md", "delegation-nested.md", "subagent.md", "ship-note.md"]
GROWTH_CAP = 1.05  # dilution guard: variant total chars <= champion * cap
STABLE_LO, STABLE_HI = 0.25, 0.75  # pooled pass-rate band that marks a task noisy
PROMOTE_BAR = 2.0  # stable_delta + 0.5*noisy_delta must reach this to promote
CONFIRM_BAR = 2    # end-of-night: champion must beat baseline by >= this at n=6


def log(msg):
    print(f"[tune {time.strftime('%H:%M:%S')}] {msg}", flush=True)


def variant_chars(d):
    return sum(len((d / f).read_text()) for f in SECTION_FILES if (d / f).exists())


def read_meta(d):
    p = d / "meta.json"
    return json.loads(p.read_text()) if p.exists() else None


def write_meta(d, meta):
    (d / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")


def load_rows(path):
    if not path.exists():
        return []
    out = []
    for line in path.read_text().splitlines():
        try:
            out.append(json.loads(line))
        except ValueError:
            continue
    return out


def rows_for(name):
    return [r for r in load_rows(results_file) if r.get("variant") == name]


def score(name) -> "dict[str, Any] | None":
    rows = rows_for(name)
    if not rows:
        return None
    passes = sum(r["pass"] for r in rows)
    cost = sum(r.get("cost_usd") or 0 for r in rows)
    return {
        "n": len(rows),
        "passes": passes,
        "pass_rate": passes / len(rows),
        "cost_usd": round(cost, 4),
        "cost_per_solved": round(cost / passes, 4) if passes else None,
        "fail_reasons": dict(Counter(r["fail_reason"] for r in rows if not r["pass"])),
    }


def task_detail(name):
    per = {}
    for r in rows_for(name):
        d = per.setdefault(r["task"], {"n": 0, "pass": 0, "fails": [], "sessions_failed": []})
        d["n"] += 1
        d["pass"] += r["pass"]
        if not r["pass"]:
            d["fails"].append(r.get("fail_reason") or "unclassified")
            if r.get("session"):
                d["sessions_failed"].append(r["session"])
    return per


# ---- 4. task-variance ledger -------------------------------------------------

def stable_tasks():
    """Tasks whose pooled builtin-prompt pass rate sits outside the coin-flip band.

    Pools the main bench's bough rows (results.jsonl, no variant = builtin) with
    every campaign's baseline rows — but only rows from the model this campaign
    runs (noise profiles don't transfer between models). Tasks with <6 pooled
    trials count as stable (no evidence of noisiness yet).
    """
    model = os.environ.get("BENCH_MODEL_BOUGH", "claude-haiku-4-5")
    pooled = defaultdict(lambda: [0, 0])
    for r in load_rows(BENCH / "results" / "results.jsonl"):
        if r.get("harness") == "bough" and not r.get("variant") and r.get("model") == model:
            pooled[r["task"]][0] += r["pass"]
            pooled[r["task"]][1] += 1
    for camp in (BENCH / "results").glob("tune-*.jsonl"):
        for r in load_rows(camp):
            if r.get("variant") == "baseline" and r.get("model") == model:
                pooled[r["task"]][0] += r["pass"]
                pooled[r["task"]][1] += 1
    stable, noisy = set(), set()
    for task, (p, n) in pooled.items():
        rate = p / n
        (noisy if n >= 6 and STABLE_LO < rate < STABLE_HI else stable).add(task)
    return stable, noisy


def paired_delta(challenger, champion, noisy):
    """Per-task paired comparison; returns (stable_delta, noisy_delta, moved)."""
    ch, cm = task_detail(challenger), task_detail(champion)
    sd = nd = 0
    moved = []
    for task in set(ch) & set(cm):
        d = ch[task]["pass"] - cm[task]["pass"]
        if d:
            moved.append(task)
        if task in noisy:
            nd += d
        else:
            sd += d
    return sd, nd, moved


def promotes(challenger, champion, noisy):
    sd, nd, moved = paired_delta(challenger, champion, noisy)
    return sd + 0.5 * nd >= PROMOTE_BAR, sd, nd, moved


# ---- 1. failed-trial transcripts (GEPA-style reflection) ----------------------

def transcript_excerpt(session_id, max_chars=2400):
    """Compact trace of a bench session: agent text, programs, tool output."""
    try:
        con = sqlite3.connect(f"file:{BENCH_DB}?mode=ro", uri=True)
        rows = con.execute(
            "select role, parts from messages where session_id=? order by created_at",
            (session_id,)).fetchall()
        con.close()
    except sqlite3.Error as e:
        return f"(transcript unavailable: {e})"
    lines = []
    for role, parts in rows:
        try:
            parsed = json.loads(parts)
        except ValueError:
            continue
        for p in parsed:
            t = p.get("type")
            if t == "text" and p.get("text"):
                lines.append(f"[{role}] {p['text'][:600]}")
            elif t == "tool_call":
                code = (p.get("input") or {}).get("code", "")
                lines.append(f"[program]\n{code[:700]}")
            elif t == "tool_result":
                err = " (ERROR)" if p.get("isError") else ""
                lines.append(f"[output{err}] {str(p.get('output', ''))[:500]}")
    text = "\n".join(lines)
    # Keep head and tail: the head has the task framing, the tail has whatever
    # the agent believed when it stopped — both matter for diagnosis.
    if len(text) > max_chars:
        half = max_chars // 2
        text = text[:half] + "\n[... trimmed ...]\n" + text[-half:]
    return text


def failed_examples(variant, k=3):
    """Up to k failed trials of `variant`, spread across distinct fail reasons."""
    seen, out = set(), []
    for r in rows_for(variant):
        if r["pass"] or not r.get("session"):
            continue
        key = (r.get("fail_reason"), r["task"])
        if key in seen:
            continue
        seen.add(key)
        out.append((r["task"], r.get("fail_reason") or "unclassified", r["session"]))
        if len(out) >= k:
            break
    return out


# ---- 3. persistent learnings ---------------------------------------------------

def learn(entry):
    with open(LEARNINGS, "a") as fh:
        fh.write(f"- [{time.strftime('%Y-%m-%d')}] {entry}\n")


def learnings_text(max_lines=80):
    if not LEARNINGS.exists():
        return ""
    return "\n".join(LEARNINGS.read_text().splitlines()[-max_lines:])


# ---- 5. Pareto frontier ---------------------------------------------------------

def pareto_frontier(min_n=2):
    """variant -> tasks it is strictly best at (current campaign, min_n trials)."""
    best = {}
    per = defaultdict(dict)  # task -> variant -> rate
    for p in VARIANTS.iterdir():
        if not p.is_dir():
            continue
        for task, d in task_detail(p.name).items():
            if d["n"] >= min_n:
                per[task][p.name] = d["pass"] / d["n"]
    for task, rates in per.items():
        top = max(rates.values())
        winners = [v for v, r in rates.items() if r == top]
        if len(winners) == 1:
            best.setdefault(winners[0], []).append(task)
    return best


def pick_merge_partner(champion):
    """Frontier member that owns the most tasks the champion doesn't win."""
    frontier = pareto_frontier()
    frontier.pop(champion, None)
    frontier.pop("baseline", None)
    if not frontier:
        return None
    return max(frontier.items(), key=lambda kv: len(kv[1]))


# ---- sweeps ---------------------------------------------------------------------

def collect_online_seeds():
    """Queued online-ACE candidates not yet raced in this campaign: variant dirs
    with meta source=='online' and no rows in the current results file. Ordered by
    the queue manifest online.py writes, newest campaigns first, then by name."""
    queue = VARIANTS / "_online-queue.json"
    order = []
    if queue.exists():
        try:
            order = json.loads(queue.read_text())
        except ValueError:
            order = []
    seeds = []
    for name in list(order) + sorted(p.name for p in VARIANTS.iterdir() if p.is_dir()):
        if name in seeds:
            continue
        d = VARIANTS / name
        m = read_meta(d)
        if m and m.get("source") == "online" and not rows_for(name):
            seeds.append(name)
    return seeds


def seed_baseline():
    d = VARIANTS / "baseline"
    if not (d / "system.md").exists():
        env = {k: v for k, v in os.environ.items() if k != "BOUGH_PROMPT_DIR"}
        subprocess.run(
            ["deno", "run", "--allow-read", "--allow-write", "--allow-env",
             str(TUNE / "dump-prompt.ts"), str(d)],
            check=True, env=env, cwd=BENCH.parent,
        )
        write_meta(d, {"name": "baseline", "hypothesis": "built-in prompt as of seed time",
                       "prediction": None, "parent": None, "ts": int(time.time())})
        log(f"seeded baseline ({variant_chars(d)} chars)")
    return d


def sweep(name, trials, tasks):
    d = VARIANTS / name
    LOGS.mkdir(parents=True, exist_ok=True)
    logf = LOGS / f"{name}.log"
    env = dict(os.environ,
               BOUGH_PROMPT_DIR=str(d),
               BENCH_VARIANT=name,
               BENCH_RESULTS_FILE=str(results_file),
               # Reuse the campaign's one server; the variant's prompt rides per
               # session via exec.ts --prompt-dir (run-bough.sh), so no restart.
               BENCH_KEEP_SERVER="1")
    cmd = [str(BENCH / "run.sh"), "-n", str(trials), "-H", "bough", *tasks]
    log(f"sweep {name}: {trials} trial(s) x {len(tasks) or 'all'} tasks -> {logf.name}")
    t0 = time.time()
    with open(logf, "a") as fh:
        rc = subprocess.run(cmd, stdout=fh, stderr=fh, env=env).returncode
    dt = time.time() - t0
    if rc != 0:
        log(f"sweep {name} FAILED (exit {rc}) — see {logf}")
        return None, dt
    return score(name), dt


# ---- proposer ---------------------------------------------------------------------

def extract_json(text):
    try:
        return json.loads(text)
    except ValueError:
        pass
    m = re.search(r"```(?:json)?\s*(\{.*?\})\s*```", text, re.S)
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
    raise ValueError("no JSON object in proposer reply")


def variant_files_block(name):
    d = VARIANTS / name
    parts = []
    for f in SECTION_FILES:
        if (d / f).exists():
            parts.append(f"\n## FILE {f}\n\n{(d / f).read_text()}\n")
    return "".join(parts)


def propose(champion, noisy, proposer_model, merge_partner=None):
    parts = [(TUNE / "proposer.md").read_text()]
    if merge_partner:
        partner, owned = merge_partner
        parts.append(
            f"\n\n# MODE: MERGE\n\nProduce ONE variant that merges the champion "
            f"({champion}) with {partner}, which currently beats it on: "
            f"{', '.join(owned)}. Keep what makes each strong; resolve conflicts "
            f"in the champion's favor. Same output contract, same growth cap "
            f"(measured against the champion).")
    parts.append(f"\n\n# Champion: {champion} ({variant_chars(VARIANTS / champion)} chars total)\n")
    parts.append(variant_files_block(champion))
    if merge_partner:
        parts.append(f"\n\n# Merge partner: {merge_partner[0]}\n")
        parts.append(variant_files_block(merge_partner[0]))
    parts.append(f"\n# Champion bench score\n\n{json.dumps(score(champion), indent=2)}\n")
    parts.append("\n# Per-task detail (champion)\n")
    for task, d in sorted(task_detail(champion).items()):
        noise = " [NOISY task — do not target: coin-flip for this model]" if task in noisy else ""
        fails = f" fails: {', '.join(d['fails'])}" if d["fails"] else ""
        parts.append(f"- {task}: {d['pass']}/{d['n']}{fails}{noise}")
    for task, reason, session in failed_examples(champion):
        parts.append(f"\n# Failed-trial transcript: {task} ({reason})\n")
        parts.append(transcript_excerpt(session))
    lt = learnings_text()
    if lt:
        parts.append("\n\n# Learnings from past campaigns (do not re-propose refuted directions)\n")
        parts.append(lt)
    cmd = ["claude", "-p", "--output-format", "json"]
    if proposer_model:
        cmd += ["--model", proposer_model]
    r = subprocess.run(cmd, input="\n".join(parts), capture_output=True, text=True, timeout=600)
    if r.returncode != 0:
        raise RuntimeError(f"proposer exit {r.returncode}: {r.stderr[:500]}")
    prop = extract_json(json.loads(r.stdout)["result"])
    files = prop.get("files") or {}
    bad = set(files) - set(SECTION_FILES)
    if bad or not files:
        raise ValueError(f"proposer files invalid: {sorted(bad) or 'empty'}")
    return prop


def materialize(champion, prop):
    name = re.sub(r"[^a-z0-9-]", "-", prop["name"].lower()).strip("-") or "variant"
    d = VARIANTS / name
    n = 2
    while d.exists():
        d = VARIANTS / f"{name}-{n}"
        n += 1
    name = d.name
    d.mkdir(parents=True)
    champ_dir = VARIANTS / champion
    for f in SECTION_FILES:
        src = prop.get("files", {}).get(f)
        if src is None and (champ_dir / f).exists():
            src = (champ_dir / f).read_text()
        if src is not None:
            (d / f).write_text(src.rstrip() + "\n")
    return name, d


def append_prediction(meta, outcome, result):
    line = {
        "ts": meta["ts"],
        "edit": f"tune/{meta['name']}: {meta['hypothesis']}",
        "prediction": meta["prediction"],
        "baseline": f"{results_file.name} variant={meta['parent']}",
        "verify": "bench/tune/tune.py race (pre-registered in variant meta.json before the sweep)",
        "outcome": outcome,
        "result": result,
    }
    with open(BENCH / "predictions.jsonl", "a") as fh:
        fh.write(json.dumps(line) + "\n")


# ---- end-of-night confirmation ------------------------------------------------

def confirm_champion(champion, campaign, trials=6):
    """Champion vs baseline at n=6 on the tasks that moved; >= CONFIRM_BAR passes
    to keep the promotion. Runs in its own results file — different trial counts
    must never pool with the campaign's."""
    global results_file
    _, noisy = stable_tasks()
    _, _, moved = paired_delta(champion, "baseline", noisy)
    if not moved:
        return True, "no moved tasks — nothing to confirm"
    saved = results_file
    results_file = BENCH / "results" / f"tune-{campaign}-confirm.jsonl"
    try:
        log(f"confirming {champion} vs baseline at n={trials} on: {', '.join(moved)}")
        sweep("baseline", trials, moved)
        sweep(champion, trials, moved)
        b, c = score("baseline"), score(champion)
        if not b or not c:
            return False, "confirmation sweep failed"
        delta = c["passes"] - b["passes"]
        verdict = delta >= CONFIRM_BAR
        detail = f"{c['passes']}/{c['n']} vs baseline {b['passes']}/{b['n']} (delta {delta:+d}, bar +{CONFIRM_BAR})"
        return verdict, detail
    finally:
        results_file = saved


# ---- main -----------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--hours", type=float, default=8.0, help="wall-clock budget (default 8)")
    ap.add_argument("--trials", type=int, default=3, help="full-race trials per task (default 3)")
    ap.add_argument("--tasks", nargs="*", default=[], help="task subset (default: all)")
    ap.add_argument("--max-variants", type=int, default=20, help="challenger cap for the night")
    ap.add_argument("--proposer-model", default=None, help="model for claude -p proposer")
    ap.add_argument("--sweep-only", metavar="NAME", help="just (re-)sweep an existing variant and exit")
    ap.add_argument("--seed-online", action="store_true",
                    help="race queued online-ACE candidates (bench/tune/online.py) as the "
                         "first challengers, before LLM-proposing new ones")
    ap.add_argument("--campaign", default=time.strftime("%Y-%m-%d"),
                    help="results bucket (default: today); sweeps append to results/tune-<campaign>.jsonl")
    args = ap.parse_args()

    global results_file
    results_file = BENCH / "results" / f"tune-{args.campaign}.jsonl"
    deadline = time.time() + args.hours * 3600
    VARIANTS.mkdir(parents=True, exist_ok=True)
    seed_baseline()

    # One server for the whole campaign: restart ONCE now so it runs current code,
    # then keep it (sweeps set BENCH_KEEP_SERVER=1). Per-variant prompts ride
    # exec.ts --prompt-dir, so no variant pays a server restart — the old loop
    # restarted per sweep.
    subprocess.run([str(BENCH / "server.sh"), "stop"], capture_output=True)
    subprocess.run([str(BENCH / "server.sh"), "start"], check=True)

    if args.sweep_only:
        s, _ = sweep(args.sweep_only, args.trials, args.tasks)
        print(json.dumps(s, indent=2))
        return

    stable, noisy = stable_tasks()
    log(f"task ledger: {len(stable)} stable, noisy = {sorted(noisy) or 'none'}")

    if score("baseline") is None:
        s, _ = sweep("baseline", args.trials, args.tasks)
        if s is None:
            sys.exit("baseline sweep failed — fix the bench before tuning")
        log(f"baseline: {s['passes']}/{s['n']} @ ${s['cost_per_solved']}/solved")

    champion = "baseline"
    seeds = collect_online_seeds() if args.seed_online else []
    if seeds:
        log(f"seeding {len(seeds)} queued online candidate(s): {', '.join(seeds)}")
    sweep_est, tried, failures = 1500.0, 0, 0
    while time.time() + sweep_est * 1.2 < deadline and tried < args.max_variants:
        # Queued online-ACE candidates race first (their dir + meta already exist);
        # once drained, fall back to LLM-proposing fresh challengers.
        seed = seeds.pop(0) if seeds else None
        merge_partner = None if seed else (
            pick_merge_partner(champion) if tried and tried % 3 == 2 else None)
        if seed:
            name, d = seed, VARIANTS / seed
            meta = read_meta(d) or {"name": name, "hypothesis": "", "prediction": "",
                                    "mode": "online", "ts": int(time.time())}
            meta["parent"] = champion  # judged against the current champion
        else:
            try:
                prop = propose(champion, noisy, args.proposer_model, merge_partner)
                name, d = materialize(champion, prop)
            except Exception as e:  # noqa: BLE001 — overnight loop must survive one bad reply
                failures += 1
                log(f"proposer failed ({failures}): {e}")
                if failures >= 3:
                    log("3 consecutive proposer failures — stopping")
                    break
                continue
            meta = {"name": name, "hypothesis": prop.get("hypothesis", ""),
                    "prediction": prop.get("prediction", ""), "parent": champion,
                    "mode": "merge" if merge_partner else "mutate",
                    "changed": sorted(prop.get("files", {}).keys()), "ts": int(time.time())}
        failures = 0
        tried += 1
        champ_chars = variant_chars(VARIANTS / champion)
        if variant_chars(d) > champ_chars * GROWTH_CAP:
            meta["outcome"] = "rejected-growth"
            write_meta(d, meta)
            learn(f"{name}: {meta['hypothesis']} -> REJECTED pre-sweep, grew prompt "
                  f"{variant_chars(d)} > {champ_chars} chars (+5% cap)")
            log(f"{name}: rejected (growth)")
            continue
        write_meta(d, meta)  # prediction pre-registered before any sweep runs

        # Stage 1: screen at n=1 across all tasks.
        s1, dt1 = sweep(name, 1, args.tasks)
        if s1 is None:
            meta["outcome"] = "sweep-error"
            write_meta(d, meta)
            continue
        champ_score = score(champion)
        assert champ_score is not None
        screen_bar = champ_score["pass_rate"] - 0.15
        if s1["pass_rate"] < screen_bar:
            meta["outcome"] = "refuted-screen"
            meta["score"] = s1
            write_meta(d, meta)
            append_prediction(meta, "refuted", f"screen n=1: {s1['passes']}/{s1['n']} "
                              f"(bar {screen_bar:.0%}) — not escalated")
            learn(f"{name}: {meta['hypothesis']} -> refuted at n=1 screen "
                  f"({s1['passes']}/{s1['n']} vs champion rate {champ_score['pass_rate']:.0%})")
            log(f"{name}: screened out ({s1['passes']}/{s1['n']})")
            sweep_est = 0.7 * sweep_est + 0.3 * dt1 * (args.trials)
            continue

        # Stage 2: escalate to full trials.
        s, dt2 = sweep(name, args.trials - 1, args.tasks)
        sweep_est = 0.7 * sweep_est + 0.3 * (dt1 + dt2)
        if s is None:
            meta["outcome"] = "sweep-error"
            write_meta(d, meta)
            continue

        promoted, sd, nd, moved = promotes(name, champion, noisy)
        meta["score"] = s
        meta["paired"] = {"stable_delta": sd, "noisy_delta": nd, "moved": moved}
        meta["outcome"] = "promoted" if promoted else "refuted"
        write_meta(d, meta)
        result = (f"{s['passes']}/{s['n']} vs champion {champ_score['passes']}/{champ_score['n']}; "
                  f"paired stable {sd:+d} noisy {nd:+d} (bar {PROMOTE_BAR}) "
                  f"-> {'PROMOTED' if promoted else 'not promoted'}")
        append_prediction(meta, "confirmed" if promoted else "refuted", result)
        learn(f"{name} ({meta['mode']}): {meta['hypothesis']} -> {result}; moved: {', '.join(moved) or 'nothing'}")
        log(f"{name}: {result}")
        if promoted:
            champion = name

    # End-of-night: a champion only survives its n=6 confirmation.
    confirmed, confirm_detail = False, "no challenger beat baseline"
    if champion != "baseline":
        ok, detail = confirm_champion(champion, args.campaign)
        meta = read_meta(VARIANTS / champion) or {"name": champion, "ts": int(time.time()),
                                                  "hypothesis": "", "prediction": "", "parent": "baseline"}
        meta["confirmation"] = detail
        meta["outcome"] = "confirmed" if ok else "refuted-on-confirmation"
        write_meta(VARIANTS / champion, meta)
        append_prediction(meta, meta["outcome"], f"n=6 confirmation: {detail}")
        learn(f"{champion} end-of-night confirmation: {detail} -> "
              f"{'CONFIRMED, adoption-grade' if ok else 'REFUTED — night gain was variance'}")
        confirmed, confirm_detail = ok, detail
        if not ok:
            log(f"champion {champion} FAILED confirmation ({detail}) — reverting to baseline")
            champion = "baseline"

    # Machine-readable outcome for the continuous pipeline (bench/tune/nightly.sh):
    # a confirmed, non-baseline champion is the signal to open an adoption PR.
    summary = {
        "campaign": args.campaign,
        "champion": champion,
        "confirmed": bool(confirmed and champion != "baseline"),
        "detail": confirm_detail,
        "challengers": tried,
        "results_file": results_file.name,
        "hypothesis": (read_meta(VARIANTS / champion) or {}).get("hypothesis", ""),
    }
    (BENCH / "results" / f"tune-{args.campaign}-summary.json").write_text(
        json.dumps(summary, indent=2) + "\n")

    subprocess.run([str(BENCH / "server.sh"), "stop"], capture_output=True)
    log(f"done: {tried} challenger(s), final champion = {champion} "
        f"(confirmed={summary['confirmed']})")
    subprocess.run([sys.executable, str(TUNE / "report.py"), str(results_file)])


if __name__ == "__main__":
    main()
