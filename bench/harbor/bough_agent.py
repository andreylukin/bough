"""bough (the rebuild) as a Harbor installed agent — Terminal-Bench 2.x and any Harbor dataset.

    export PYTHONPATH=$PWD/bench/harbor
    harbor run --dataset terminal-bench@2.0 \
      --agent-import-path bough_agent:Bough \
      --model anthropic/claude-haiku-4-5-20251001 \
      --ak binary=$PWD/bench/pier/dist/bough-linux-x86_64 \
      --jobs-dir ~/.cache/bough-tbench/jobs -k 5 --n-concurrent 4

Two things about the REBUILD shape this file, and both are simpler than the old adapter's:

1. **`bough exec` is in-process.** The headless profile mounts the whole tree inside the one
   command; there is no server to boot and poll. One command per attempt, that is all.

2. **The ledger is the continuity.** `bough exec` "resumes-or-creates" the agent on the ledger
   under `$BOUGH_HOME`, so attempt 2 in the same container reads the same lane: the projection it
   is sent carries attempt 1's steps, tiers and pins. The continuation prompt says the clock cut
   the last attempt off; it does not have to explain what happened, the context does.

What has not changed: there are no prebuilt bough binaries, so a Linux binary built once on the
host (`bench/pier/build-linux-binary.sh`, x86_64 for Terminal-Bench 2.0's amd64 images) is
uploaded into every trial; and the three clocks — Harbor's cap on the agent phase, the budget
for all attempts, the turn — are ordered so the phase is never shot mid-attempt.

Usage is read from the ledger's `usage/round` steps in the JSON envelope (`--print json` prints
the answer wake's steps), and the status from `wake/end.reason`. A turn the clock killed prints
no envelope; its usage is recovered from `ledger.db`, which is downloaded into the trial's agent
dir together with `requests/` (every request the model was sent, verbatim — `request.recorder`).
"""

from __future__ import annotations

import json
import os
import shlex
import sqlite3
import time
import tomllib
from pathlib import Path
from typing import Any, override

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

BINARY_PATH = "/installed-agent/bough"
HOME_PATH = "/installed-agent/home"
PATCH_PATH = "/installed-agent/bough.patch.yml"

# Slack under Harbor's cap so the phase is never killed mid-attempt, and under each turn's own
# clock so `bough exec` prints its envelope before the shell around it is cut off.
CAP_RESERVE = 60
TURN_MARGIN = 30
# The shortest turn worth starting: under a tight cap one long attempt beats two stunted ones.
MIN_TURN = 400

# Benchmark hosts a model must not consult (the answer key lives there), plus the raw GitHub
# hosts a leaked solution would come from. Loopback in /etc/hosts; the bench's own rule.
_BLOCK_BENCHMARK_HOSTS = (
    "for h in tbench.ai www.tbench.ai api.github.com raw.githubusercontent.com "
    'gist.githubusercontent.com; do echo "127.0.0.1 $h" >> /etc/hosts; '
    'echo "::1 $h" >> /etc/hosts; done'
)

# What the container's model costs per million tokens, for the ledger's `usage/round.cost_usd`
# (`model-policy.prices`). Unknown models get no entry and report cost as unknown, never 0.
_PRICES: dict[str, dict[str, float]] = {
    # OpenRouter /api/v1/models, 2026-08-31. Z.ai caching is implicit, writes bill as input.
    "openrouter:z-ai/glm-5.3-flash": {
        "input_per_mtok": 0.075,
        "output_per_mtok": 0.25,
        "cache_read_per_mtok": 0.015,
        "cache_write_per_mtok": 0.075,
    },
    "openrouter:z-ai/glm-5.3": {
        "input_per_mtok": 1.4,
        "output_per_mtok": 4.4,
        "cache_read_per_mtok": 0.26,
        "cache_write_per_mtok": 1.4,
    },
    "claude-haiku-4-5-20251001": {
        "input_per_mtok": 1.0,
        "output_per_mtok": 5.0,
        "cache_read_per_mtok": 0.1,
        "cache_write_per_mtok": 1.25,
    },
}

_FORWARDED_ENV = (
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
)


class Bough(BaseInstalledAgent):
    """bough-next, driven headlessly through `bough exec --print json`."""

    # bough writes its own ledger, not ATIF; tokens and cost reach Harbor through AgentContext.
    SUPPORTS_ATIF: bool = False
    SUPPORTS_RESUME: bool = False
    SUPPORTS_WINDOWS: bool = False

    def __init__(
        self,
        *args,
        binary: str | None = None,
        timeout: int = 1800,
        attempts: int = 2,
        budget: int | None = None,
        cap: int | None = None,
        patch: str | None = None,
        skills: str | None = None,
        **kwargs,
    ):
        super().__init__(*args, **kwargs)
        self._binary = Path(binary).expanduser() if binary else None
        # An evolved skill set (WikiSkill loop, 2026-08-31): a directory of
        # `<name>/SKILL.md` dirs uploaded into the trial home's `skills/`, where the
        # skills row mounts them (catalog + `skill` tool). The trial sees SKILLS ONLY —
        # never the tuner's wiki (the paper's ablation).
        self._skills = Path(skills).expanduser() if skills else None
        self._timeout = int(timeout)
        self._attempts = max(int(attempts), 1)
        self._budget = int(budget) if budget else None
        self._cap = int(cap) if cap else None
        # An ARM: a patch file over the bundles (projection knobs, rollups, …). Uploaded and
        # layered under the model patch this adapter writes.
        self._patch = Path(patch).expanduser() if patch else None
        if not self._binary or not self._binary.is_file():
            raise ValueError(
                "bough has no published binaries: pass --ak binary=/path/to/linux/bough "
                "(bench/pier/build-linux-binary.sh; x86_64 for Terminal-Bench 2.0's images)"
            )
        if self._patch and not self._patch.is_file():
            raise ValueError(f"--ak patch: no such file: {self._patch}")
        if self._skills and not self._skills.is_dir():
            raise ValueError(f"--ak skills: no such directory: {self._skills}")

    @staticmethod
    @override
    def name() -> str:
        return "bough"

    @override
    def get_version_command(self) -> str | None:
        return f"{shlex.quote(BINARY_PATH)} --version"

    @override
    def parse_version(self, stdout: str) -> str:
        return stdout.strip().removeprefix("bough").strip()

    # ---- install ---------------------------------------------------------

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        await self.exec_as_root(environment, command=_BLOCK_BENCHMARK_HOSTS)
        # What bough shells out to by name. No node: the rebuild runs the programs the model
        # writes in its own embedded QuickJS. No libssl: the binary links OpenSSL statically.
        await self.ensure_system_dependencies(
            environment, ("curl", "git", "ripgrep", "ca_certificates", "procps")
        )
        await environment.upload_file(self._binary, BINARY_PATH)
        await self.exec_as_root(
            environment,
            command=(
                f"chmod 0755 {shlex.quote(BINARY_PATH)} && "
                f"ln -sf {shlex.quote(BINARY_PATH)} /usr/local/bin/bough && "
                f"mkdir -p {shlex.quote(HOME_PATH)} && chmod 0777 {shlex.quote(HOME_PATH)}"
            ),
        )
        # Prove the binary runs NOW: an arch or libc mismatch surfaces here as one clear line
        # rather than later as "nohup: No such file or directory".
        probe = await environment.exec(command=f"{shlex.quote(BINARY_PATH)} --version")
        if probe.return_code != 0:
            raise RuntimeError(
                f"bough binary does not run in this container (exit {probe.return_code}): "
                f"{(probe.stderr or probe.stdout or '').strip()[:400]} — "
                "does its arch match the image? (uname -m)"
            )
        await self._write_patch(environment)
        await self._upload_skills(environment)

    async def _upload_skills(self, environment: BaseEnvironment) -> None:
        if not self._skills:
            return
        count = 0
        for skill_md in sorted(self._skills.glob("*/SKILL.md")):
            name = skill_md.parent.name
            remote = f"{HOME_PATH}/skills/{name}/SKILL.md"
            await self.exec_as_root(
                environment, command=f"mkdir -p {shlex.quote(f'{HOME_PATH}/skills/{name}')}"
            )
            await environment.upload_file(skill_md, remote)
            count += 1
        self.logger.info(f"bough: uploaded {count} evolved skill(s)")

    async def _write_patch(self, environment: BaseEnvironment) -> None:
        """The model, as a patch over `model.policy`, plus the caller's arm patch if any.

        A patch layer replaces a row's WHOLE config map, so the prices go with the model.
        """
        model = self._bough_model()
        layers: list[str] = []
        if self._patch:
            layers.append(self._patch.read_text())
        if model:
            prices = {model: _PRICES[model]} if model in _PRICES else {}
            cfg = {"sol": model, "terra": model, "prices": prices}
            layers.append(
                "entries:\n  model.policy:\n    config: " + json.dumps(cfg) + "\n"
            )
        if not layers:
            return
        # Two documents cannot share one file; the arm goes first as its own file, the model
        # patch second — later layers win.
        paths = []
        for i, text in enumerate(layers):
            path = f"{PATCH_PATH}.{i}" if len(layers) > 1 else PATCH_PATH
            local = self.logs_dir / f"patch-{i}.yml"
            local.parent.mkdir(parents=True, exist_ok=True)
            local.write_text(text)
            await environment.upload_file(local, path)
            paths.append(path)
        self._patch_paths = paths

    _patch_paths: list[str] = []

    # ---- run -------------------------------------------------------------

    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        env = self._agent_env()
        budget, attempts, turn = self._plan()
        deadline = time.monotonic() + budget
        results: list[Any] = []
        envelopes: list[dict[str, Any]] = []
        last_status: str | None = None

        for attempt in range(1, attempts + 1):
            left = deadline - time.monotonic()
            if attempt > 1 and left < turn + CAP_RESERVE:
                self.logger.info(
                    f"bough: stopping after {attempt - 1} attempt(s), {left:.0f}s of budget left"
                )
                break
            if attempt == 1:
                prompt = instruction
            elif last_status == "announced":
                prompt = self._nudge()
            else:
                prompt = self._continuation(instruction)
            this_turn = min(turn, max(int(left) - TURN_MARGIN, 60))
            patches = " ".join(f"--patch {shlex.quote(p)}" for p in self._patch_paths)
            # `timeout -s INT` is the graceful ending: bough's SIGINT handler tears the tree
            # down, the ledger keeps every step, and the next attempt continues the lane.
            command = (
                f"cd /app 2>/dev/null || cd ~; "
                f"BOUGH_HOME={shlex.quote(HOME_PATH)} "
                f"timeout -s INT -k 30 {this_turn} "
                f"{shlex.quote(BINARY_PATH)} {patches} exec --print json "
                f"{shlex.quote(prompt)}"
            )
            result = await environment.exec(
                command=f"set -o pipefail; {command}",
                env=env,
                timeout_sec=this_turn + 120,
            )
            results.append(result)
            envelope = _envelope(result.stdout or "")
            status = "cut off" if result.return_code == 124 else None
            if envelope:
                envelope["attempt"] = attempt
                envelope["return_code"] = result.return_code
                envelopes.append(envelope)
                status = _status(envelope)
                self._write_agent_log(envelopes)
            else:
                # No envelope: the clock (124), a boot failure, or a crash. Keep what the
                # process said so a broken container reads as one and not as a zero.
                (self.logs_dir / f"attempt-{attempt}.txt").write_text(
                    f"exit {result.return_code}\n--- stdout\n{result.stdout or ''}\n"
                    f"--- stderr\n{result.stderr or ''}\n"
                )
                if result.return_code not in (0, 1, 124, 130):
                    raise RuntimeError(
                        f"bough exec failed to run (exit {result.return_code}): "
                        f"{(result.stderr or '').strip()[-600:]}"
                    )
            # A turn that ended `completed` has said what it has to say; re-running it would let
            # the model second-guess correct work with no test to tell the two apart. UNLESS it
            # never touched a tool: a flash-class model's opening "I'll start by reading the
            # report" with zero calls is an announcement, not an answer (GLM-5.3-flash, first
            # Modal trial, 2026-08-31) — nudge it into acting instead of scoring the narration.
            acted = bool(envelope) and any(
                s.get("kind") in ("tool/call", "program/call")
                for s in envelope.get("steps", [])
            )
            if status == "completed" and acted:
                break
            if status == "completed":
                status = "announced"
            last_status = status
            self.logger.info(
                f"bough: attempt {attempt} ended {status!r}; "
                f"{'retrying' if attempt < attempts else 'out of attempts'}"
            )

        await self._collect(environment)
        self._populate(context, results, envelopes)

    def _plan(self) -> tuple[int, int, int]:
        cap = self._cap or self._harbor_cap()
        budget = self._budget or (cap - CAP_RESERVE if cap else self._attempts * self._timeout)
        budget = max(budget, MIN_TURN)
        attempts = max(1, min(self._attempts, budget // (MIN_TURN + TURN_MARGIN)))
        divisible = budget - CAP_RESERVE if attempts > 1 else budget
        turn = max(min(self._timeout, divisible // attempts - TURN_MARGIN), 60)
        self.logger.info(
            f"bough: cap={cap or 'unknown'}s budget={budget}s attempts={attempts} turn={turn}s"
        )
        return int(budget), int(attempts), int(turn)

    def _harbor_cap(self) -> int | None:
        """The task's `[agent] timeout_sec`, from the trial's config and Harbor's task cache.
        Best-effort: a missing cache is a wider budget, not a crash."""
        try:
            config = json.loads((self.logs_dir.parent / "config.json").read_text())
            task = config["task"]["path"]
            root = Path(os.environ.get("HARBOR_TASKS_DIR", Path.home() / ".cache/harbor/tasks"))
            for toml_path in sorted(root.glob(f"**/{Path(task).name}/task.toml")):
                with toml_path.open("rb") as handle:
                    return int(tomllib.load(handle)["agent"]["timeout_sec"])
        except Exception as exc:  # noqa: BLE001 - never fail a trial over this
            self.logger.info(f"bough: could not read Harbor's agent cap: {exc}")
        return None

    def _nudge(self) -> str:
        return (
            "You ended your turn after only describing what you were going to do — no command "
            "was executed and nothing changed on disk. Do the work now: use your tools to read "
            "the files, make the changes, and run the tests. Do not end your turn again until "
            "you have actually run the verification and it passes, or you have made your best "
            "complete attempt."
        )

    def _continuation(self, instruction: str) -> str:
        return (
            "Your previous attempt at this task was stopped by the clock mid-turn; its steps "
            "are in your context above and its edits are in the working directory. Continue "
            "from where you stopped: check what is on disk, keep what is correct, finish the "
            "job, and verify the result yourself.\n\nThe task:\n\n" + instruction
        )

    def _agent_env(self) -> dict[str, str]:
        env = {"BOUGH_HOME": HOME_PATH, "NO_COLOR": "1"}
        for key in _FORWARDED_ENV:
            value = os.environ.get(key)
            if value:
                env[key] = value
        return env

    def _bough_model(self) -> str | None:
        """Harbor's `provider/model` → bough's id: a bare `claude-…` is Anthropic; the routed
        providers keep their prefix in bough's COLON spelling (`openrouter:vendor/model`)."""
        if not self.model_name:
            return None
        for provider in ("openrouter", "openai"):
            if self.model_name.startswith(f"{provider}/"):
                return f"{provider}:" + self.model_name.removeprefix(f"{provider}/")
        return self.model_name.removeprefix("anthropic/")

    # ---- results ---------------------------------------------------------

    def _write_agent_log(self, envelopes: list[dict[str, Any]]) -> None:
        path = self.logs_dir / "bough-exec.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(envelopes, indent=2))

    async def _collect(self, environment: BaseEnvironment) -> None:
        """The ledger and every recorded request, next to the envelopes. Best-effort."""
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        try:
            await environment.download_file(f"{HOME_PATH}/ledger.db", self.logs_dir / "ledger.db")
        except Exception as exc:  # noqa: BLE001
            self.logger.info(f"bough: no ledger downloaded: {exc}")
        try:
            await environment.download_dir(f"{HOME_PATH}/requests", self.logs_dir / "requests")
        except Exception as exc:  # noqa: BLE001
            self.logger.info(f"bough: no requests downloaded: {exc}")

    def _populate(
        self, context: AgentContext, results: list[Any], envelopes: list[dict[str, Any]]
    ) -> None:
        """Tokens and cost SUMMED over every attempt, from the ledger when it is here (a turn
        the clock killed printed no envelope) and from the envelopes otherwise."""
        usage = _usage_from_ledger(self.logs_dir / "ledger.db")
        if usage is None:
            usage = _usage_from_steps(
                [s for e in envelopes for s in e.get("steps", [])]
            )
        context.n_input_tokens = usage["input"]
        context.n_output_tokens = usage["output"]
        context.n_cache_tokens = usage["cache_read"]
        context.cost_usd = usage["cost"]
        last = envelopes[-1] if envelopes else None
        context.metadata = {
            "attempts": len(results),
            "return_codes": [r.return_code for r in results],
            "status": _status(last) if last else "no envelope",
            "wakes": [e.get("wake") for e in envelopes],
            "rounds": usage["rounds"],
            "cost_known": usage["cost_known"],
        }


# ---- pure helpers ---------------------------------------------------------


def _envelope(stdout: str) -> dict[str, Any] | None:
    """`bough exec --print json` prints one object: `{"wake": …, "steps": […]}`."""
    text = stdout.strip()
    start = text.find("{")
    if start < 0:
        return None
    try:
        value = json.loads(text[start:])
    except json.JSONDecodeError:
        # Something printed after it: take the outermost object that parses.
        end = text.rfind("}")
        try:
            value = json.loads(text[start : end + 1])
        except (json.JSONDecodeError, ValueError):
            return None
    return value if isinstance(value, dict) and "steps" in value else None


def _status(envelope: dict[str, Any] | None) -> str:
    if not envelope:
        return "no envelope"
    for step in reversed(envelope.get("steps", [])):
        if step.get("kind") == "wake/end":
            body = step.get("body") or {}
            reason = str(body.get("reason") or "unknown").lower()
            cause = body.get("cause")
            return f"{reason}" + (f" ({cause})" if cause else "")
    return "no wake/end"


def _usage_from_steps(steps: list[dict[str, Any]]) -> dict[str, Any]:
    totals: dict[str, Any] = {"input": 0, "output": 0, "cache_read": 0, "cost": 0.0, "rounds": 0}
    known = True
    for step in steps:
        if step.get("kind") != "usage/round":
            continue
        body = step.get("body") or {}
        totals["rounds"] += 1
        totals["input"] += int(body.get("input_tokens") or 0)
        totals["output"] += int(body.get("output_tokens") or 0)
        totals["cache_read"] += int(body.get("cache_read_tokens") or 0)
        cost = body.get("cost_usd")
        if cost is None:
            known = False
        else:
            totals["cost"] += float(cost)
    totals["cost_known"] = known
    if not known and totals["cost"] == 0.0:
        totals["cost"] = None
    return totals


def _usage_from_ledger(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    try:
        conn = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        rows = conn.execute("SELECT body FROM steps WHERE type = 'usage/round'").fetchall()
        conn.close()
    except sqlite3.Error:
        return None
    steps = [{"kind": "usage/round", "body": json.loads(body)} for (body,) in rows]
    return _usage_from_steps(steps)
