"""The Go bough (go/) as a Harbor installed agent — Terminal-Bench 4.0 on Modal.

    cd go && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o ../bench/harbor/dist/bough-go-linux-amd64 ./cmd/bough
    export PYTHONPATH=$PWD/bench/harbor
    harbor run -d terminal-bench/terminal-bench@4.0.0 --env modal \
      --agent bough_go_agent:BoughGo --model openrouter/openai/gpt-5.6-luna \
      --ak binary=$PWD/bench/harbor/dist/bough-go-linux-amd64 --ak timeout=2400 \
      -i html-js-filter -k 3 --n-concurrent 3 --jobs-dir ~/.cache/bough-tbench/jobs

One `bough -headless` process per trial: the task brief goes in as one JSON prompt line, the
loop's events come out as `[kind] text` lines, and the cost row prints `[usage] {...}` after the
turn. The agent phase is capped by `timeout` (TB 4.0's own limit is 8 h) — one attempt, with
`-c` continuation available if a second attempt is ever wanted.
"""

from __future__ import annotations

import json
import os
import shlex
from pathlib import Path
from typing import override

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

BINARY_PATH = "/installed-agent/bough"
HOME_PATH = "/installed-agent/home"
CONFIG_PATH = "/installed-agent/bough.yml"
PROMPT_PATH = "/installed-agent/prompt.jsonl"

# Benchmark hosts a model must not consult (the answer key lives there), plus the raw GitHub
# hosts a leaked solution would come from. Loopback in /etc/hosts; the bench's own rule.
_BLOCK_BENCHMARK_HOSTS = (
    "for h in tbench.ai www.tbench.ai api.github.com raw.githubusercontent.com "
    'gist.githubusercontent.com; do echo "127.0.0.1 $h" >> /etc/hosts; '
    'echo "::1 $h" >> /etc/hosts; done'
)

_FORWARDED_ENV = ("OPENROUTER_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY")

# The tree a trial mounts: provider row from --model, then the daily-driver's plugins minus
# anything that needs a GUI or the host (no init-js, no mcp, no graph, no skills catalog).
_CONFIG = """\
- id: llm
  plugin: {plugin}
  config:
    model: {model}
- id: cost
  plugin: cost
- id: codemode
  plugin: codemode
- id: commands
  plugin: commands
- id: tools
  plugin: tools-basic
- id: workers
  plugin: workers
- id: history
  plugin: history
- id: todo
  plugin: todo
- id: loop
  plugin: loop
- id: ui
  plugin: ui
"""


def _provider(model: str) -> tuple[str, str]:
    """Harbor's --model ("openrouter/openai/gpt-5.6-luna", "anthropic/claude-…") → (plugin, model)."""
    if model.startswith("openrouter/"):
        return "llm-openrouter", model.removeprefix("openrouter/")
    if model.startswith("anthropic/"):
        return "llm-anthropic", model.removeprefix("anthropic/")
    if model.startswith("openai/"):
        return "llm-openai", model.removeprefix("openai/")
    return "llm-openrouter", model


class BoughGo(BaseInstalledAgent):
    """The Go bough, driven through `bough -headless`."""

    SUPPORTS_ATIF: bool = False
    SUPPORTS_RESUME: bool = False
    SUPPORTS_WINDOWS: bool = False

    def __init__(
        self,
        *args,
        binary: str | None = None,
        timeout: int = 2400,
        config: str | None = None,
        **kwargs,
    ):
        super().__init__(*args, **kwargs)
        self._binary = Path(binary).expanduser() if binary else None
        self._timeout = int(timeout)
        # An ARM: a whole config tree instead of the default one (prompt/plugin experiments).
        self._config = Path(config).expanduser() if config else None
        if not self._binary or not self._binary.is_file():
            raise ValueError(
                "pass --ak binary=/path/to/bough-go-linux-amd64 "
                "(cd go && CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build ./cmd/bough)"
            )
        if self._config and not self._config.is_file():
            raise ValueError(f"--ak config: no such file: {self._config}")

    @staticmethod
    @override
    def name() -> str:
        return "bough-go"

    @override
    def get_version_command(self) -> str | None:
        return f"{shlex.quote(BINARY_PATH)} -version"

    @override
    def parse_version(self, stdout: str) -> str:
        return stdout.strip().removeprefix("bough").strip()

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        await self.exec_as_root(environment, command=_BLOCK_BENCHMARK_HOSTS)
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
        probe = await environment.exec(command=f"{shlex.quote(BINARY_PATH)} -version")
        if probe.return_code != 0:
            raise RuntimeError(
                f"bough binary does not run in this container (exit {probe.return_code}): "
                f"{(probe.stderr or probe.stdout or '').strip()[:400]}"
            )
        if self._config:
            text = self._config.read_text()
        else:
            plugin, model = _provider(self.model_name or "openrouter/openai/gpt-5.6-luna")
            text = _CONFIG.format(plugin=plugin, model=model)
        local = self.logs_dir / "bough.yml"
        local.parent.mkdir(parents=True, exist_ok=True)
        local.write_text(text)
        await environment.upload_file(local, CONFIG_PATH)

    def _agent_env(self) -> dict[str, str]:
        env = {k: v for k in _FORWARDED_ENV if (v := os.environ.get(k))}
        env["HOME"] = HOME_PATH
        env["TERM"] = "dumb"
        return env

    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        prompt = self.logs_dir / "prompt.jsonl"
        prompt.write_text(json.dumps({"prompt": instruction}) + "\n")
        await environment.upload_file(prompt, PROMPT_PATH)
        turn = self._timeout
        command = (
            f"cd /app 2>/dev/null || cd ~; "
            f"timeout -s INT -k 30 {turn} "
            f"{shlex.quote(BINARY_PATH)} -headless -config {shlex.quote(CONFIG_PATH)} "
            f"< {shlex.quote(PROMPT_PATH)}"
        )
        result = await environment.exec(
            command=command, env=self._agent_env(), timeout_sec=turn + 120
        )
        out = result.stdout or ""
        err = result.stderr or ""
        (self.logs_dir / "stdout.txt").write_text(out)
        (self.logs_dir / "stderr.txt").write_text(err)
        (self.logs_dir / "exit.txt").write_text(f"{result.return_code}\n")
        # Usage: the last "[usage] {...}" line is the cumulative total.
        usage = None
        for line in out.splitlines():
            if line.startswith("[usage] "):
                try:
                    usage = json.loads(line[len("[usage] ") :])
                except json.JSONDecodeError:
                    pass
        status = "cut off" if result.return_code == 124 else "done"
        if usage:
            context.n_input_tokens = int(usage.get("input_tokens", 0))
            context.n_output_tokens = int(usage.get("output_tokens", 0))
            if usage.get("priced"):
                context.cost_usd = float(usage.get("cost_usd", 0.0))
        context.metadata = {
            "status": status,
            "exit": result.return_code,
            "turns": out.count("[done]"),
            "code_blocks": out.count("[code]"),
            "errors": out.count("[error]") + err.count("[error]"),
        }
        if result.return_code not in (0, 1, 124, 130) and not out:
            raise RuntimeError(
                f"bough failed to run (exit {result.return_code}): {err.strip()[-600:]}"
            )
