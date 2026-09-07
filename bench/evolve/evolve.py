#!/usr/bin/env python3
"""Harness evolution for the Go bough on Terminal-Bench (AHE-shaped, HarnessCompass-gated).

The base model is frozen. What evolves are the harness components in bench/evolve/harness/:

    prompt.md     the whole base system prompt (loop config system_prompt)
    guidance.md   the bench guidance appended to it (loop config task_guidance)
    knobs.json    effort / js_tool / max_cost — the config knobs

One iteration = evaluate the candidate on the TRAIN tasks → digest every trial (failing
tests, how the turn ended, the last actions) → a meta-agent (`claude -p`) proposes ONE edit
to ONE component with a written prediction of which tasks flip → evaluate the edited harness
on train, and if train improved, on VAL → keep it only if val did not drop (the
generalization gate) → commit or revert harness/. TB 4.0 is the held-out test set and is
never run here (bench/evolve/split.json).

    bench/evolve/evolve.py baseline [--train-job DIR]   # score the current harness on train + val
    bench/evolve/evolve.py iterate [-n N]     # N iterations (default 1)
    bench/evolve/evolve.py digest <job-dir>   # print the digest the meta-agent would read
    bench/evolve/evolve.py status             # the iteration log

Env: K (trials per task, default 2), CONC (default 10), META (the proposer model; default claude-fable-5-1).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
EVOLVE = ROOT / "bench" / "evolve"
HARNESS = EVOLVE / "harness"
ITER_DIR = EVOLVE / "iterations"
STATE = EVOLVE / "state.json"
SPLIT = json.loads((EVOLVE / "split.json").read_text())
JOBS = Path.home() / ".cache" / "bough-tbench" / "jobs"
TB4 = ROOT / "bench" / "harbor" / "tb4.sh"
BIN = ROOT / "bench" / "harbor" / "dist" / "bough-go-evolve"
K = int(os.environ.get("K", "2"))
CONC = os.environ.get("CONC", "10")
META = os.environ.get("META", "claude-fable-5-1")

sys.path.insert(0, str(ROOT / "bench" / "harbor"))
from summarize import _load, _reward  # noqa: E402


# ---------------------------------------------------------------- evaluate

def knobs() -> dict:
    return json.loads((HARNESS / "knobs.json").read_text())


def evaluate(tag: str, tasks: list[str]) -> Path:
    """Run the current harness on tasks, K trials each. Returns the job dir."""
    job = f"ev-{tag}"
    if (JOBS / job).exists():
        raise SystemExit(f"job {job} exists; pick another tag or remove it")
    kn = knobs()
    env = dict(os.environ)
    env.update(
        DATASET=SPLIT["dataset"], BIN=str(BIN), CONC=CONC, TIMEOUT=str(kn.get("timeout", 3600)),
        PROMPT=str(HARNESS / "prompt.md"), GUIDANCE=str(HARNESS / "guidance.md"),
    )
    if kn.get("effort"):
        env["EFFORT"] = kn["effort"]
    if kn.get("js_tool"):
        env["JSTOOL"] = "1"
    if kn.get("max_cost"):
        env["MAXCOST"] = str(kn["max_cost"])
    log = ITER_DIR / f"{job}.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("w") as f:
        subprocess.run([str(TB4), "run", job, str(K), *tasks], env=env, stdout=f, stderr=subprocess.STDOUT, check=False)
    return JOBS / job


def score(job: Path) -> tuple[float, dict[str, list[int]]]:
    """Mean reward over trials (a missing reward is 0), and per-task rewards."""
    per: dict[str, list[int]] = {}
    for trial in sorted(p for p in job.iterdir() if p.is_dir()):
        res = _load(trial / "result.json")
        if not res:
            continue
        task = trial.name.rsplit("__", 1)[0]
        r = _reward(res)
        per.setdefault(task, []).append(int(r or 0))
    n = sum(len(v) for v in per.values())
    return (sum(sum(v) for v in per.values()) / n if n else 0.0), per


# ---------------------------------------------------------------- digest

_ENDINGS = (
    ("cap", "step budget spent"),
    ("cost", "cost budget spent"),
    ("cancelled", "[cancelled]"),
)


def _ending(stdout: str) -> str:
    """How the turn ended: cap/cost/cancelled, or 'announce' (last reply promised work and ran
    nothing), 'error-last' (the last block failed), else 'claimed'."""
    for name, marker in _ENDINGS:
        if marker in stdout:
            return name
    replies = re.findall(r"^\[assistant\] ?(.*?)(?=^\[(?:code|result|error|done|assistant|thinking)\]|\Z)", stdout, re.S | re.M)
    last = replies[-1].strip() if replies else ""
    if re.search(r"\b(I'll|I will|Let me|Next,|Now I)\b", last[:200]) and "```" not in last:
        return "announce"
    if re.search(r"^\[error\]", stdout.rsplit("[code]", 1)[-1], re.M):
        return "error-last"
    return "claimed"


def _failing_tests(verifier: Path) -> list[str]:
    out = verifier / "test-stdout.txt"
    if not out.is_file():
        return ["(no verifier output)"]
    text = out.read_text(errors="replace")
    fails = re.findall(r"^(?:FAILED|ERROR) (\S+)", text, re.M)
    if not fails:
        m = re.search(r"(\d+) failed", text)
        fails = [f"({m.group(1)} failed, names not parsed)"] if m else ["(no FAILED lines; see verifier)"]
    return [f.split(" - ")[0] for f in fails][:12]


def _last_actions(stdout: str, n: int = 8) -> list[str]:
    acts = re.findall(r"^\[code\] ?(.*?)(?=^\[(?:code|result|error|done|assistant|thinking)\]|\Z)", stdout, re.S | re.M)
    return [a.strip().replace("\n", " ")[:160] for a in acts[-n:]]


def digest(job: Path) -> str:
    mean, per = score(job)
    lines = [f"# Digest of {job.name}: mean reward {mean:.3f} over {sum(len(v) for v in per.values())} trials", ""]
    lines.append("Per task: " + ", ".join(f"{t} {sum(v)}/{len(v)}" for t, v in sorted(per.items())))
    lines.append("")
    for trial in sorted(p for p in job.iterdir() if p.is_dir()):
        res = _load(trial / "result.json")
        if not res:
            continue
        task = trial.name.rsplit("__", 1)[0]
        r = int(_reward(res) or 0)
        ar = res.get("agent_result") or {}
        meta = ar.get("metadata") or {}
        exc = (res.get("exception_info") or {}).get("exception_type")
        stdout = (trial / "agent" / "stdout.txt").read_text(errors="replace") if (trial / "agent" / "stdout.txt").is_file() else ""
        lines.append(f"## {task} — {'PASS' if r else 'FAIL'}; steps {meta.get('code_blocks')}, errors {meta.get('errors')}, ${ar.get('cost_usd') or 0:.2f}, ending: {_ending(stdout)}{', exception: ' + exc if exc else ''}")
        if not r:
            lines.append("failing tests: " + "; ".join(_failing_tests(trial / "verifier")))
            lines.append("last actions:")
            lines.extend(f"  - {a}" for a in _last_actions(stdout))
        lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------- propose

PROPOSER = """You are evolving the harness around a frozen coding model (GPT-5.6 Luna via OpenRouter)
on Terminal-Bench 2.0 tasks. The harness has three editable components:

- prompt.md: the whole base system prompt (how to act: js fences, tools.*, finishing rules)
- guidance.md: the extra brief for graded tasks, appended to the prompt
- knobs.json: {"effort": off|low|medium|high|xhigh|"", "js_tool": bool, "max_cost": USD or null}

Rules (they are the point, not decoration):
1. Propose exactly ONE edit to ONE component. Do not touch the others.
2. The edit must be GENERAL: a procedure or rule that would help on unseen terminal tasks.
   Never name a task, a file from a task, or a specific answer. A rule that only helps
   one task is overfitting and will be rejected by the held-out gate.
3. Ground it in the digest: cite which failure pattern you are attacking and which
   tasks you predict will flip to PASS (and any you fear will flip to FAIL). Your
   prediction is checked next iteration; a proposer whose predictions do not hold is
   discounted.
4. Keep the prompt tight. Deletions and rewrites of weak paragraphs count as edits;
   a longer prompt is not a better one. Prefer a concrete procedure ("run X first,
   print Y") over a policy ("be careful about Z") — with this model procedures work
   and policies do not.
5. Do not restate the Terminal-Bench rules or the tools' contract; they already work.

Current components follow, then the digest of the last evaluation, then the iteration
log (what was tried, what was kept). Reply with ONE fenced json block and nothing else:

```json
{"component": "prompt.md" | "guidance.md" | "knobs.json",
 "rationale": "<3-6 sentences: the failure pattern, the mechanism of the fix>",
 "predicted_flips": {"to_pass": ["task", ...], "to_fail": ["task", ...]},
 "content": "<the COMPLETE new content of that component>"}
```
"""


def propose(dig: str, log: list[dict]) -> dict:
    parts = [PROPOSER]
    for name in ("prompt.md", "guidance.md", "knobs.json"):
        parts.append(f"\n===== {name} =====\n{(HARNESS / name).read_text()}")
    parts.append("\n===== digest =====\n" + dig)
    parts.append("\n===== iteration log =====\n" + json.dumps(log, indent=1)[-6000:])
    cmd = ["claude", "-p", "--output-format", "json", "--model", META]
    # A nested claude session refuses to start while the parent's markers are in the
    # environment, and a stale ANTHROPIC_API_KEY overrides the login.
    env = {k: v for k, v in os.environ.items() if not k.startswith("CLAUDE") and k != "ANTHROPIC_API_KEY"}
    out = subprocess.run(cmd, input="\n".join(parts), capture_output=True, text=True, check=True, env=env).stdout
    result = json.loads(out[out.index("{"):]).get("result", "")
    m = re.search(r"```json\s*(\{.*\})\s*```", result, re.S)
    if not m:
        raise RuntimeError(f"proposer returned no json block:\n{result[:2000]}")
    prop = json.loads(m.group(1))
    if prop["component"] not in ("prompt.md", "guidance.md", "knobs.json"):
        raise RuntimeError(f"bad component {prop['component']}")
    if prop["component"] == "knobs.json":
        json.loads(prop["content"]) if isinstance(prop["content"], str) else None
    return prop


# ---------------------------------------------------------------- loop

def _git(*args: str) -> str:
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True, check=True).stdout.strip()


def _state() -> dict:
    return json.loads(STATE.read_text()) if STATE.is_file() else {"best": None, "log": []}


def _save(st: dict) -> None:
    STATE.write_text(json.dumps(st, indent=1) + "\n")


def baseline(train_job: str | None) -> None:
    """Score the current harness on train + val. --train-job reuses a finished train run
    (a screen arm whose knobs harness/knobs.json now carries) instead of running it again."""
    st = _state()
    tag = dt.datetime.now().strftime("base-%m%d-%H%M")
    tr = Path(train_job).expanduser() if train_job else evaluate(tag + "-train", SPLIT["train"])
    tr_mean, tr_per = score(tr)
    va = evaluate(tag + "-val", SPLIT["val"])
    va_mean, va_per = score(va)
    st["best"] = {"tag": tag, "train": tr_mean, "val": va_mean, "train_job": str(tr), "val_job": str(va), "commit": _git("rev-parse", "--short", "HEAD")}
    st["log"].append({"iter": 0, "tag": tag, "kept": True, "train": tr_mean, "val": va_mean, "note": "baseline"})
    _save(st)
    print(f"baseline: train {tr_mean:.3f} val {va_mean:.3f}")


def iterate(n: int) -> None:
    for _ in range(n):
        st = _state()
        if not st["best"]:
            raise SystemExit("run `evolve.py baseline` first")
        i = len(st["log"])
        tag = f"i{i}-" + dt.datetime.now().strftime("%m%d-%H%M")
        it = ITER_DIR / tag
        it.mkdir(parents=True, exist_ok=True)

        dig = digest(Path(st["best"]["train_job"]))
        (it / "digest.md").write_text(dig)
        prop = propose(dig, st["log"])
        (it / "proposal.json").write_text(json.dumps(prop, indent=1))
        comp = HARNESS / prop["component"]
        before = comp.read_text()
        content = prop["content"] if isinstance(prop["content"], str) else json.dumps(prop["content"], indent=1)
        comp.write_text(content.rstrip("\n") + "\n")
        print(f"[{tag}] proposal on {prop['component']}: {prop['rationale'][:200]}")

        tr = evaluate(tag + "-train", SPLIT["train"])
        tr_mean, tr_per = score(tr)
        entry = {"iter": i, "tag": tag, "component": prop["component"], "rationale": prop["rationale"],
                 "predicted": prop.get("predicted_flips"), "train": tr_mean, "best_train": st["best"]["train"]}
        # Falsify the prediction against what actually flipped on train.
        _, best_per = score(Path(st["best"]["train_job"]))
        flipped_up = [t for t, v in tr_per.items() if sum(v) > sum(best_per.get(t, [0]))]
        flipped_down = [t for t, v in tr_per.items() if sum(v) < sum(best_per.get(t, [0]))]
        entry["flipped"] = {"up": flipped_up, "down": flipped_down}

        kept = False
        if tr_mean > st["best"]["train"]:
            va = evaluate(tag + "-val", SPLIT["val"])
            va_mean, _ = score(va)
            entry["val"] = va_mean
            entry["best_val"] = st["best"]["val"]
            if va_mean >= st["best"]["val"]:
                kept = True
                st["best"] = {"tag": tag, "train": tr_mean, "val": va_mean, "train_job": str(tr), "val_job": str(va)}
        entry["kept"] = kept
        if kept:
            _git("add", str(comp))
            _git("commit", "-q", "-m", f"evolve {tag}: {prop['component']} — train {tr_mean:.3f} val {entry['val']:.3f}\n\n{prop['rationale']}")
            st["best"]["commit"] = _git("rev-parse", "--short", "HEAD")
        else:
            comp.write_text(before)
        st["log"].append(entry)
        _save(st)
        print(f"[{tag}] train {tr_mean:.3f} (best {entry['best_train']:.3f}) " + (f"val {entry['val']:.3f} " if "val" in entry else "") + ("KEPT" if kept else "reverted") + f"; flipped up {flipped_up} down {flipped_down}")


def status() -> None:
    st = _state()
    print(json.dumps(st["best"], indent=1))
    for e in st["log"]:
        print(f"{e['iter']:>3} {e['tag']:18s} {e.get('component', '-'):12s} train {e['train']:.3f} val {e.get('val', float('nan')):.3f} {'KEPT' if e['kept'] else 'no'}")


def main() -> None:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    bl = sub.add_parser("baseline")
    bl.add_argument("--train-job")
    it = sub.add_parser("iterate")
    it.add_argument("-n", type=int, default=1)
    dg = sub.add_parser("digest")
    dg.add_argument("job")
    sub.add_parser("status")
    a = ap.parse_args()
    if a.cmd == "baseline":
        baseline(a.train_job)
    elif a.cmd == "iterate":
        iterate(a.n)
    elif a.cmd == "digest":
        print(digest(Path(a.job).expanduser()))
    else:
        status()


if __name__ == "__main__":
    main()
