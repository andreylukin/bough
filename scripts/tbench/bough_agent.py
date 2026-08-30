"""bough as a terminal-bench installed agent.

The adapter copies a LINUX build of the bough binary into the task container, points
$BOUGH_HOME at the mounted /agent-logs directory, and runs `bough exec <instruction>`.
Because the home is the mounted logs dir, the run leaves its WHOLE transcript on the
host: `ledger.db` (every step), `requests/*.md` (every request verbatim, per round,
via the request-recorder row). That is the artifact the iteration loop reads.

Usage (from the bough checkout):

    CARGO_TARGET_DIR=target-linux cargo build --release -p bough   # in rust:bookworm, see docs
    PYTHONPATH=$PWD/scripts/tbench tb run \
        --dataset-path ~/repos/terminal-bench/original-tasks \
        --task-id analyze-access-logs \
        --agent-import-path bough_agent:BoughAgent \
        --model openai:gpt-5.6-luna

The model kwarg becomes bough's model.policy for both lanes, written as the run home's
own user patch (`bough.patch.yml`), so nothing in the checkout changes per run.
"""

import os
from pathlib import Path

from terminal_bench.agents.installed_agents.abstract_installed_agent import (
    AbstractInstalledAgent,
)
from terminal_bench.terminal.models import TerminalCommand
from terminal_bench.terminal.tmux_session import TmuxSession
from terminal_bench.agents.base_agent import AgentResult

BOUGH_LINUX_BIN = Path(
    os.environ.get(
        "BOUGH_LINUX_BIN",
        str(Path.home() / "repos/bough-rebuild/target-linux/release/bough"),
    )
)


class BoughAgent(AbstractInstalledAgent):
    @staticmethod
    def name() -> str:
        return "bough"

    def __init__(self, model_name: str | None = None, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._model_name = model_name or "openai:gpt-5.6-luna"

    @property
    def _env(self) -> dict[str, str]:
        env = {"BOUGH_MODEL": self._model_name}
        for key in ("ANTHROPIC_API_KEY", "OPENAI_API_KEY"):
            if key in os.environ:
                env[key] = os.environ[key]
        return env

    @property
    def _install_agent_script_path(self) -> Path:
        return Path(__file__).parent / "bough-setup.sh"

    def _run_agent_commands(self, instruction: str) -> list[TerminalCommand]:
        import shlex

        return [
            TerminalCommand(
                command=f"BOUGH_HOME=/agent-logs/bough-home bough exec {shlex.quote(instruction)}",
                min_timeout_sec=0.0,
                max_timeout_sec=float("inf"),
                block=True,
                append_enter=True,
            )
        ]

    def perform_task(
        self,
        instruction: str,
        session: TmuxSession,
        logging_dir: Path | None = None,
    ) -> AgentResult:
        if not BOUGH_LINUX_BIN.exists():
            raise FileNotFoundError(
                f"no linux bough binary at {BOUGH_LINUX_BIN}; build one "
                "(docs/tbench.md) or set BOUGH_LINUX_BIN"
            )
        session.copy_to_container(
            BOUGH_LINUX_BIN,
            container_dir="/installed-agent",
            container_filename="bough-bin",
        )
        # THE PROMPT KNOB: every skill file beside this adapter rides into the run home's
        # skills/ dir (the setup script installs them), where bough's skills row injects a
        # skill's body into the projection when the instruction mentions one of its triggers.
        # Iterating on the prompt = editing scripts/tbench/skills/*.md, no rebuild.
        skills_dir = Path(__file__).parent / "skills"
        for skill in sorted(skills_dir.glob("*.md")):
            session.copy_to_container(
                skill,
                container_dir="/installed-agent/skills",
                container_filename=skill.name,
            )
        return super().perform_task(instruction, session, logging_dir)
