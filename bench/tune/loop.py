#!/usr/bin/env python3
"""The WikiSkill loop over TB4 on Modal (drivability, 2026-08-31; arXiv 2608.27454).

One iteration = validate the CURRENT skill set with a Modal batch, gate it, consolidate the
trial evidence into the wiki (Maintainer wake), then propose the next skill set (Proposer wake):

    batch (harbor, --env modal, skills injected)  →  gate skills_k vs best  →
    maintainer `bough exec` on the tuner lane     →  proposer `bough exec`  →  skills_{k+1}

The tuner is ONE bough lane on a persistent home (`bench/tune/home`), in the streams setup:
batch reports arrive as its wake messages; its workspace (`bench/tune/workspace`, its own git
repo) carries wiki/ + skills/ — the wiki is never reverted, skills/ are (git), matching the
paper's "wiki persists through rejected proposals". Trial agents see SKILLS ONLY, never the
wiki (the paper's ablation).

    bench/tune/loop.py iterate          # one full iteration (batch + gate + wakes)
    bench/tune/loop.py iterate -n 3     # three
    bench/tune/loop.py ingest <job-dir> # gate + wakes over an ALREADY-FINISHED batch
    bench/tune/loop.py report <job-dir> # print the batch report (what the maintainer gets)
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import sqlite3
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TUNE = ROOT / "bench" / "tune"
WORKSPACE = TUNE / "workspace"
HOME = TUNE / "home"
JOBS = Path.home() / ".cache" / "bough-tbench" / "jobs"
BINARY = ROOT / "bench" / "pier" / "dist" / "bough-linux-x86_64"

# The sol-shaped hard set from the 2026-08-31 heatmap, plus one sanity-floor task.
TASKS = [
    "mvcc-lsm-compaction",
    "kv-live-surgery",
    "wal-recovery-ordering",
    "sglang-qwen-burst",
    "vpp-loss-divergence",
    "distributed-dedup",
    "jax-speedrun-gpu",
    "payments-pipeline-fix",
]
DATASET = "terminal-bench/terminal-bench@latest"
K = int(os.environ.get("TUNE_K", "5"))
EXEC_MODEL = os.environ.get("TUNE_EXEC_MODEL", "openrouter/z-ai/glm-5.3-flash")
TUNER_MODEL = os.environ.get("TUNE_TUNER_MODEL", "openrouter:z-ai/glm-5.3")


def sh(cmd: list[str], **kw: object) -> "subprocess.CompletedProcess[bytes]":
    print("+", " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run([str(c) for c in cmd], check=True, **kw)


def git_ws(*args: str, check: bool = True) -> str:
    out = subprocess.run(
        ["git", "-C", str(WORKSPACE), *args], capture_output=True, text=True, check=check
    )
    return out.stdout.strip()


# ---- batch -------------------------------------------------------------------------------------


def batch_cmd(skills_dir: Path) -> list[str]:
    return [
        "harbor", "run", "-d", DATASET,
        *[f for t in TASKS for f in ("-i", f"terminal-bench/{t}")],
        "--agent-import-path", "bough_agent:Bough",
        "--model", EXEC_MODEL,
        "--ak", f"binary={BINARY}",
        "--ak", "attempts=3",
        "--ak", f"skills={skills_dir}",
        "--env", "modal", "-k", str(K), "-n", os.environ.get("TUNE_N", "48"), "-y",
        # Trials that die to client-side transport (local DNS flaps breaking Modal streams)
        # are retried whole rather than polluting the gate (2026-08-31 baseline: 9/40).
        "--max-retries", "2", "--retry-include", "ConnectionError",
        "--jobs-dir", str(JOBS),
    ]


def run_batch() -> Path:
    return run_batches([WORKSPACE / "skills"])[0]


def run_batches(skills_dirs: list[Path]) -> list[Path]:
    """One Modal batch per skills dir, ALL CONCURRENT — Modal is the free axis, the wall clock
    is still one batch. Returns the job dirs in the same order as the inputs."""
    before = {p.name for p in JOBS.iterdir()} if JOBS.exists() else set()
    env = {**os.environ, "PYTHONPATH": str(ROOT / "bench" / "harbor")}
    procs = []
    for d in skills_dirs:
        cmd = batch_cmd(d)
        print("+", " ".join(cmd), flush=True)
        procs.append(subprocess.Popen(cmd, env=env))
    fails = [i for i, p in enumerate(procs) if p.wait() != 0]
    if fails:
        raise SystemExit(f"batch process(es) {fails} failed")
    new = sorted({p.name for p in JOBS.iterdir()} - before)
    if len(new) < len(skills_dirs):
        raise SystemExit(f"expected {len(skills_dirs)} new job dirs, found {new}")
    # Job dirs are timestamped at start; starts happen in launch order.
    return [JOBS / n for n in new[: len(skills_dirs)]]


# ---- reading a job -----------------------------------------------------------------------------


def trial_dirs(job: Path) -> list[Path]:
    return sorted(p for p in job.iterdir() if p.is_dir() and "__" in p.name)


def trial_row(trial: Path) -> dict[str, object]:
    task = trial.name.split("__")[0]
    reward = None
    result = trial / "result.json"
    if result.is_file():
        data = json.loads(result.read_text())
        # Harbor 0.22's trial shape: verifier_result.rewards.reward.
        rewards = (data.get("verifier_result") or {}).get("rewards") or {}
        reward = rewards.get("reward")
    ends: list[str] = []
    calls = 0
    ledger = trial / "agent" / "ledger.db"
    if ledger.is_file():
        try:
            db = sqlite3.connect(f"file:{ledger}?mode=ro", uri=True)
            ends = [
                json.loads(b).get("reason") or "?"
                for (b,) in db.execute("SELECT body FROM steps WHERE type='wake/end' ORDER BY seq")
            ]
            calls = db.execute(
                "SELECT COUNT(*) FROM steps WHERE type IN ('tool/call','program/call')"
            ).fetchone()[0]
            db.close()
        except Exception as exc:  # noqa: BLE001
            ends = [f"ledger unreadable: {exc}"]
    return {
        "task": task,
        "trial": trial.name,
        "reward": reward,
        "wake_ends": ends,
        "calls": calls,
        "artifacts": str(trial / "agent"),
    }


def batch_rows(job: Path) -> list[dict[str, object]]:
    return [trial_row(t) for t in trial_dirs(job)]


def mean_reward(rows: list[dict[str, object]]) -> float:
    scored = [r["reward"] for r in rows if isinstance(r["reward"], (int, float))]
    return sum(scored) / len(scored) if scored else 0.0


def render_report(job: Path) -> str:
    rows = batch_rows(job)
    by_task: dict[str, list[dict[str, object]]] = {}
    for r in rows:
        by_task.setdefault(r["task"], []).append(r)
    lines = [
        f"Batch report — job {job.name}, {len(rows)} trials, mean reward {mean_reward(rows):.3f}.",
        "Per task (reward per trial · wake-end reasons · tool calls · artifact dir):",
        "",
    ]
    for task, rs in sorted(by_task.items()):
        lines.append(f"## {task}  ({sum(1 for r in rs if r['reward'] == 1)}/{len(rs)} pass)")
        for r in rs:
            ends = ",".join(r["wake_ends"]) or "-"
            lines.append(
                f"- {r['trial']}: reward={r['reward']} ends=[{ends}] calls={r['calls']}\n"
                f"  artifacts: {r['artifacts']}"
            )
        lines.append("")
    return "\n".join(lines)


# ---- gate --------------------------------------------------------------------------------------


def gate(job: Path) -> None:
    rows = batch_rows(job)
    score = mean_reward(rows)
    # A batch that mostly failed to RUN judges nothing: VOID, keep skills, keep the wiki.
    ran = sum(1 for r in rows if (r["calls"] or 0) > 0)
    if rows and ran < len(rows) // 2:
        impact = WORKSPACE / "wiki" / "skill-impact.md"
        with impact.open("a") as f:
            f.write(
                f"\n## {dt.datetime.now().strftime('%Y-%m-%d %H:%M')} · job {job.name}\n"
                f"- verdict: VOID — only {ran}/{len(rows)} trials ran any tool; infra, not skills\n"
            )
        print(f"gate: VOID ({ran}/{len(rows)} trials ran); nothing accepted or reverted")
        return
    best_path = WORKSPACE / "best.json"
    best = json.loads(best_path.read_text())["score"] if best_path.is_file() else None
    dirty = bool(git_ws("status", "--porcelain", "--", "skills"))
    accepted = best is None or score >= best
    rev = git_ws("rev-parse", "--short", "HEAD")
    diff = git_ws("diff", "HEAD", "--stat", "--", "skills") or "(no skill change)"
    stamp = dt.datetime.now().strftime("%Y-%m-%d %H:%M")
    impact = WORKSPACE / "wiki" / "skill-impact.md"
    with impact.open("a") as f:
        f.write(
            f"\n## {stamp} · job {job.name}\n"
            f"- skills at: {rev}{' + uncommitted' if dirty else ''}\n"
            f"- mean reward: {score:.3f} (best so far: {best if best is not None else '—'})\n"
            f"- verdict: {'ACCEPTED' if accepted else 'REJECTED'}\n"
            f"- diff:\n```\n{diff}\n```\n"
            f"- per task: "
            + ", ".join(
                f"{t}={sum(1 for r in rows if r['task'] == t and r['reward'] == 1)}"
                f"/{sum(1 for r in rows if r['task'] == t)}"
                for t in sorted({r['task'] for r in rows})
            )
            + "\n"
        )
    if accepted:
        best_path.write_text(json.dumps({"score": score, "job": job.name}))
        git_ws("add", "-A")
        if git_ws("status", "--porcelain"):
            git_ws("commit", "-qm", f"iteration over {job.name}: mean {score:.3f} ACCEPTED")
        print(f"gate: ACCEPTED at {score:.3f}")
    else:
        # Skills roll back; the wiki NEVER does (the paper's critical ablation). `checkout`
        # restores tracked edits; `clean` removes untracked new skills — a first proposal is
        # entirely untracked and `checkout` alone errors on it (2026-09-01).
        git_ws("checkout", "--", "skills", check=False)
        git_ws("clean", "-fdq", "--", "skills")
        git_ws("add", "wiki")
        if git_ws("status", "--porcelain", "--", "wiki"):
            git_ws("commit", "-qm", f"iteration over {job.name}: mean {score:.3f} REJECTED (wiki kept)")
        print(f"gate: REJECTED at {score:.3f} (best {best:.3f}); skills reverted, wiki kept")


# ---- the tuner lane's wakes --------------------------------------------------------------------


def tuner_patch() -> Path:
    HOME.mkdir(parents=True, exist_ok=True)
    patch = HOME / "bough.patch.yml"
    patch.write_text(
        "entries:\n  model.policy:\n    config:\n"
        f"      interactive: {TUNER_MODEL}\n      unattended: {TUNER_MODEL}\n      prices: {{}}\n"
    )
    return patch


def wake(prompt_file: Path, message: str, expect: str) -> None:
    """One tuner-lane turn. `expect` is a workspace path that MUST show edits afterward; a turn
    that analyzed in prose and wrote nothing gets exactly one nudge retry — the same
    announces-instead-of-acting backstop the bench adapter carries (iteration 1's Maintainer
    ran `pwd`/`ls`, narrated a good RCA, and concluded without touching a file)."""
    tuner_patch()
    env = {**os.environ, "BOUGH_HOME": str(HOME)}
    prompt = prompt_file.read_text() + "\n\n---\n\n" + message
    for round_ in range(2):
        sh(["bough", "exec", "--print", "text", prompt], cwd=WORKSPACE, env=env)
        if git_ws("status", "--porcelain", "--", expect):
            return
        print(f"wake: no edits under {expect!r}; nudging", flush=True)
        prompt = (
            f"Your previous turn analyzed but wrote NOTHING under {expect}/ — the analysis is "
            f"in your context above. Write the files now with your file tools, then summarize."
        )
    print(f"wake: still no edits under {expect!r} after a nudge; moving on", flush=True)


def ingest(job: Path) -> None:
    gate(job)
    report = render_report(job)
    wake(TUNE / "prompts" / "maintainer.md", report, "wiki")
    wake(
        TUNE / "prompts" / "proposer.md",
        f"The latest batch was job {job.name}. wiki/ and skills/ are in your working directory.",
        "skills",
    )


# ---- arms --------------------------------------------------------------------------------------


def race() -> None:
    """Race every `skills-<arm>/` variant beside `skills/` in CONCURRENT batches; install the
    best-scoring variant into `skills/` (uncommitted) and record every arm in the impact log.
    The ordinary `iterate` gate then validates the winner like any other proposal."""
    arms = sorted(p for p in WORKSPACE.glob("skills-*") if p.is_dir())
    if not arms:
        raise SystemExit("no skills-*/ arm directories to race")
    jobs = run_batches(arms)
    scored = []
    impact = WORKSPACE / "wiki" / "skill-impact.md"
    with impact.open("a") as f:
        f.write(f"\n## race · {dt.datetime.now().strftime('%Y-%m-%d %H:%M')}\n")
        for arm, job in zip(arms, jobs):
            score = mean_reward(batch_rows(job))
            scored.append((score, arm, job))
            f.write(f"- {arm.name}: mean {score:.3f} (job {job.name})\n")
    score, arm, job = max(scored, key=lambda t: t[0])
    skills = WORKSPACE / "skills"
    subprocess.run(["rm", "-rf", str(skills)], check=True)
    subprocess.run(["cp", "-R", str(arm), str(skills)], check=True)
    print(f"race: {arm.name} wins at {score:.3f}; installed into skills/ (job {job.name})")


# ---- cli ---------------------------------------------------------------------------------------


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    it = sub.add_parser("iterate")
    it.add_argument("-n", type=int, default=1)
    ing = sub.add_parser("ingest")
    ing.add_argument("job", type=Path)
    rep = sub.add_parser("report")
    rep.add_argument("job", type=Path)
    sub.add_parser("race")
    args = ap.parse_args()

    if args.cmd == "race":
        race()
    elif args.cmd == "report":
        print(render_report(args.job))
    elif args.cmd == "ingest":
        ingest(args.job)
    elif args.cmd == "iterate":
        for i in range(args.n):
            print(f"=== iteration {i + 1}/{args.n} ===")
            ingest(run_batch())


if __name__ == "__main__":
    main()
