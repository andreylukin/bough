"""bough as a Harbor installed agent — for Terminal-Bench 2.x and any other
Harbor dataset.

    harbor run --dataset terminal-bench@2.0 \
      --agent bench.harbor.bough_agent:Bough \
      --model claude-opus-4-1 \
      --ak binary=$PWD/bench/harbor/dist/bough-linux \
      -k 5 --n-concurrent 4

Two things about bough shape this file, and a naive port gets both wrong:

1. **`bough exec` is a CLIENT.** It talks to a server over loopback and does
   NOT start one (the auto-start lived in the old bash wrapper; the Rust binary
   dropped it). So `run()` boots `bough start` detached, waits for
   `GET /sessions` to answer, and only then execs. One server per container,
   started once and reused if `run()` is called twice (resume).

2. **There are no prebuilt bough binaries** — a normal install compiles the
   workspace, which is minutes per container and unacceptable × 89 tasks × 5
   trials. So the default path uploads a Linux binary you built once on the
   host (`build-linux-binary.sh`). `--ak source=1` falls back to an in-container
   source build for the rare case where you want HEAD and can pay for it.

   The binary must match the CONTAINER's architecture, which for Terminal-Bench
   2.0 is amd64 — its tasks pin prebuilt `alexgshaw/…` images. On Apple Silicon
   those run emulated, so an aarch64 binary is the wrong one even though the
   host is arm64. install() proves the binary runs before anything depends on
   it, because the failure mode otherwise is an ELF-interpreter error that
   reads like a missing file.
"""

import json
import shlex
import time
from pathlib import Path
from typing import Any, override

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from harbor.utils.env import parse_bool_env_value

# The port `bough exec` defaults to. Kept explicit so the server and the client
# cannot disagree, and so a task that happens to bind 4321 can be moved off it
# with `--ak port=…`.
DEFAULT_PORT = 4321

# Where the uploaded binary lands. /installed-agent is created by
# BaseInstalledAgent.setup() before install() runs.
BINARY_PATH = "/installed-agent/bough"

REPO = "https://github.com/andreylukin/bough.git"


class Bough(BaseInstalledAgent):
    """bough, driven headlessly through `bough exec --json`."""

    # bough emits its own transcript format, not ATIF. Token counts and cost
    # still reach Harbor through AgentContext below.
    SUPPORTS_ATIF: bool = False
    # `bough exec` opens a fresh session per invocation; there is no
    # --continue. Resume would silently start over, which is worse than
    # declining it.
    SUPPORTS_RESUME: bool = False
    SUPPORTS_WINDOWS: bool = False

    def __init__(
        self,
        *args,
        binary: str | None = None,
        source: bool = False,
        ref: str = "main",
        port: int = DEFAULT_PORT,
        timeout: int = 900,
        attempts: int = 3,
        budget: int | None = None,
        effort: str | None = None,
        **kwargs,
    ):
        super().__init__(*args, **kwargs)
        self._binary = Path(binary).expanduser() if binary else None
        # `--ak source=0` arrives as the string "0", which is truthy. Harbor's
        # own parser is the one that reads it as false.
        self._source = parse_bool_env_value(source, name="source")
        self._ref = ref
        self._port = int(port)
        self._timeout = int(timeout)
        self._attempts = max(int(attempts), 1)
        # Thinking depth. bough reads BOUGH_EFFORT at server boot, so this has
        # to be in the environment the server starts with, not a turn flag --
        # `bough exec` has none. None = let the provider decide.
        self._effort = effort
        # The wall clock for ALL attempts together. Harbor enforces its own cap
        # on the agent phase (task timeout × --agent-timeout-multiplier) and
        # kills the phase outright when it expires, so this must be set under
        # that cap — see the README's timeout arithmetic.
        self._budget = int(budget) if budget else self._attempts * self._timeout
        self._server_started = False

        if not self._binary and not self._source:
            raise ValueError(
                "bough has no published binaries: pass --ak "
                "binary=/path/to/linux/bough (see bench/harbor/build-linux-binary.sh) "
                "or --ak source=1 to compile inside every container."
            )
        if self._binary and not self._binary.is_file():
            raise ValueError(f"--ak binary: no such file: {self._binary}")

    @staticmethod
    @override
    def name() -> str:
        return "bough"

    @override
    def get_version_command(self) -> str | None:
        return f"{shlex.quote(BINARY_PATH)} --version"

    @override
    def parse_version(self, stdout: str) -> str:
        # `bough 0.4.1` → `0.4.1`
        return stdout.strip().removeprefix("bough").strip()

    # ---- install ---------------------------------------------------------

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        # bough shells out to these by name. `rg` and `ast-grep` in particular
        # are named unconditionally by the system prompt, so a container
        # without them makes the agent look broken rather than unequipped.
        await self.exec_as_root(
            environment,
            command=(
                "apt-get update && apt-get install -y --no-install-recommends "
                "ca-certificates curl git ripgrep nodejs"
            ),
            env={"DEBIAN_FRONTEND": "noninteractive"},
            timeout_sec=600,
        )

        if self._binary:
            await environment.upload_file(self._binary, BINARY_PATH)
            await self.exec_as_root(
                environment, command=f"chmod 0755 {shlex.quote(BINARY_PATH)}"
            )
        else:
            await self._build_from_source(environment)

        await self.exec_as_root(
            environment, command=f"ln -sf {shlex.quote(BINARY_PATH)} /usr/local/bin/bough"
        )
        # Prove the binary is there and runnable NOW. Harbor's own version probe
        # swallows its exception, so without this a failed upload or an
        # arch/libc mismatch surfaces much later as "nohup: No such file or
        # directory" from the server start, which reads like a PATH bug.
        await self.exec_as_root(
            environment, command=f"{shlex.quote(BINARY_PATH)} --version"
        )
        # bough writes its DB, logs and scratch under BOUGH_HOME. Create it as
        # the agent user so the server does not first touch it as root.
        await self.exec_as_agent(environment, command="mkdir -p ~/.bough")

    async def _build_from_source(self, environment: BaseEnvironment) -> None:
        await self.exec_as_root(
            environment,
            command=(
                "apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev"
            ),
            env={"DEBIAN_FRONTEND": "noninteractive"},
            timeout_sec=900,
        )
        await self.exec_as_root(
            environment,
            command=(
                "set -euo pipefail; "
                "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs "
                "| sh -s -- -y --default-toolchain stable --profile minimal"
            ),
            timeout_sec=900,
        )
        await self.exec_as_root(
            environment,
            command=(
                "set -euo pipefail; "
                'export PATH="$HOME/.cargo/bin:$PATH"; '
                f"git clone --depth 1 --branch {shlex.quote(self._ref)} {REPO} /opt/bough-src && "
                "cargo build --release --manifest-path /opt/bough-src/Cargo.toml -p bough && "
                f"install -m 0755 /opt/bough-src/target/release/bough {shlex.quote(BINARY_PATH)}"
            ),
            # A cold cargo build of the workspace is the slowest thing in this
            # file by an order of magnitude.
            timeout_sec=3600,
        )

    # ---- run -------------------------------------------------------------

    @override
    @with_prompt_template
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        env = self._agent_env()
        await self._ensure_server(environment, env)

        deadline = time.monotonic() + self._budget
        results: list[Any] = []
        envelopes: list[dict[str, Any]] = []

        for attempt in range(1, self._attempts + 1):
            # Never start an attempt that cannot finish inside the budget:
            # Harbor kills the whole agent phase at its own cap, and a phase
            # killed mid-attempt loses the envelope for every attempt before
            # it. Leaving early with results in hand beats being shot.
            left = deadline - time.monotonic()
            if attempt > 1 and left < self._timeout + 60:
                self.logger.info(
                    f"bough: stopping after {attempt - 1} attempt(s), "
                    f"{left:.0f}s left of the budget"
                )
                break

            prompt = instruction if attempt == 1 else self._continuation(instruction)
            command = " ".join(
                [
                    f"{shlex.quote(BINARY_PATH)} exec --json",
                    f"--port {self._port}",
                    f"--timeout {min(self._timeout, max(int(left) - 30, 60))}",
                    *(
                        [f"--model {shlex.quote(self._bough_model() or '')}"]
                        if self._bough_model()
                        else []
                    ),
                    "--",
                    shlex.quote(prompt),
                ]
            )

            # A turn that errors is a task the agent failed, not a harness
            # fault: exit 1 must NOT raise, or every unsolved task reads as
            # infrastructure breakage. Only exit 2 (usage / the server
            # unreachable) is ours.
            result = await environment.exec(
                command=f"set -o pipefail; {command}",
                env=env,
                timeout_sec=self._timeout + 120,
            )
            if result.return_code == 2:
                raise self._classify_exec_error(command, result)

            results.append(result)
            envelope = _last_json_object(result.stdout or "")
            if envelope:
                envelopes.append(envelope)

            # Retry only a turn that did NOT finish on its own terms. A turn
            # that reached `done` has said what it has to say; re-running it
            # would let the model second-guess correct work with no test to
            # tell it apart. A timeout or an error is the opposite: bough was
            # cut off mid-task, the container still holds the partial work,
            # and a fresh turn resumes from the filesystem.
            status = (envelope or {}).get("status")
            if status == "done":
                break
            self.logger.info(
                f"bough: attempt {attempt} ended {status!r}; "
                f"{'retrying' if attempt < self._attempts else 'out of attempts'}"
            )

        self._populate(context, results, envelopes)

    def _continuation(self, instruction: str) -> str:
        """The prompt for attempt 2+.

        `bough exec` has no session resume, so this is a fresh conversation —
        but the same container, so the previous attempt's edits are still on
        disk. The prompt has to say that, or the model starts over and burns
        the budget rediscovering what it already wrote.
        """
        return (
            "A previous attempt at this task ran out of time and was stopped "
            "mid-turn. Its work is still in the working directory — read what "
            "is there before changing anything, keep what is correct, and "
            "finish the job. Verify the result yourself (run it, test it) "
            "rather than assuming the earlier attempt got it right.\n\n"
            "The task:\n\n" + instruction
        )

    def _agent_env(self) -> dict[str, str]:
        """The provider keys plus bough's own port, from --ae/the host env.

        bough routes by model id prefix and reads the key at turn time, so the
        server process is what needs these — not `bough exec`. Both get them;
        it is one dict and the client ignores what it does not use.
        """
        env = {"BOUGH_PORT": str(self._port)}
        for key in (
            "BOUGH_EFFORT",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "CLOUDFLARE_API_KEY",
        ):
            value = self._get_env(key)
            if value:
                env[key] = value
        # LAST, so it wins: `--ak effort=` is an explicit instruction for this
        # run and must beat whatever BOUGH_EFFORT the host happens to carry.
        # Setting it before the forwarding loop let the ambient value overwrite
        # the flag, which is the wrong way round and silent when it happens.
        if self._effort:
            env["BOUGH_EFFORT"] = self._effort
        return env

    def _bough_model(self) -> str | None:
        """Harbor's `provider/model` → a bough model id.

        bough routes on the id alone: a bare `claude-…` is Anthropic, anything
        with a slash is OpenRouter, `openai:` is OpenAI proper. So Harbor's
        `anthropic/claude-opus-4-1` has to lose its prefix or it would be sent
        to OpenRouter, and `openai/gpt-5` has to become `openai:gpt-5` — but
        only when the caller meant OpenAI proper rather than OpenRouter's
        passthrough, which we cannot know, so `openai/…` is left alone and
        reaches OpenRouter. Say `--model openai:gpt-5` if you want the
        Responses API.
        """
        if not self.model_name:
            return None
        return self.model_name.removeprefix("anthropic/")

    async def _ensure_server(
        self, environment: BaseEnvironment, env: dict[str, str]
    ) -> None:
        if self._server_started:
            return
        # Detached, with stdio fully redirected: exec() waits on the pipe, not
        # just the process, so an inherited stdout keeps this call open for the
        # life of the server.
        await self.exec_as_agent(
            environment,
            command=(
                f"nohup {shlex.quote(BINARY_PATH)} start "
                ">/tmp/bough-server.log 2>&1 </dev/null & "
                "disown || true"
            ),
            env=env,
        )
        # Poll rather than sleep: a fixed sleep is either flaky or wasted, and
        # this multiplies by every trial.
        await self.exec_as_agent(
            environment,
            command=(
                "for i in $(seq 1 120); do "
                f'  curl -fsS "http://127.0.0.1:{self._port}/sessions" >/dev/null 2>&1 && exit 0; '
                "  sleep 0.5; "
                "done; "
                'echo "bough server did not answer on port '
                f'{self._port}" >&2; cat /tmp/bough-server.log >&2; exit 1'
            ),
            env=env,
            timeout_sec=120,
        )
        self._server_started = True

    # ---- results ---------------------------------------------------------

    def _populate(
        self,
        context: AgentContext,
        results: list[Any],
        envelopes: list[dict[str, Any]],
    ) -> None:
        """Fold every attempt's `--json` envelope into Harbor's context.

        Tokens and cost are SUMMED across attempts — the run paid for all of
        them, and reporting only the last one would make a three-attempt task
        look as cheap as a one-attempt task. The status and session reported
        are the last attempt's, because that is the one whose work the verifier
        is about to grade.
        """
        if not envelopes:
            self.logger.warning("bough exec produced no JSON envelope")
            context.metadata = {
                "attempts": len(results),
                "return_code": results[-1].return_code if results else None,
            }
            return

        totals = {"inputTokens": 0, "outputTokens": 0, "cacheReadTokens": 0}
        cost = 0.0
        for envelope in envelopes:
            # treeUsage collapses subagents and workflow agents under the
            # session; it is the honest number for a harness paying for all.
            usage = envelope.get("treeUsage") or envelope.get("usage") or {}
            for key in totals:
                totals[key] += usage.get(key) or 0
            cost += usage.get("costUsd") or 0.0

        last = envelopes[-1]
        context.n_input_tokens = totals["inputTokens"]
        context.n_output_tokens = totals["outputTokens"]
        context.n_cache_tokens = totals["cacheReadTokens"]
        context.cost_usd = cost
        context.metadata = {
            "session": last.get("session"),
            "status": last.get("status"),
            "ok": last.get("ok"),
            "error": last.get("error"),
            "attempts": len(results),
            "attempt_statuses": [e.get("status") for e in envelopes],
            "return_code": results[-1].return_code if results else None,
        }

        agent_log = self.logs_dir / "bough-exec.json"
        agent_log.parent.mkdir(parents=True, exist_ok=True)
        agent_log.write_text(json.dumps(envelopes, indent=2))


def _last_json_object(stdout: str) -> dict[str, Any] | None:
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            parsed = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            return parsed
    return None
