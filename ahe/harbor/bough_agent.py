"""bough as a Harbor agent, so the Terminal-Bench task bank can be reused.

WHY AN INSTALLED AGENT AND NOT AN EXTERNAL ONE. Harbor offers two shapes. An
*external* agent drives the container from outside through `environment.exec`,
which would mean reimplementing bough's whole loop in terms of someone else's
exec primitive — and bough's file verbs (`view`, `patch`, `write`) would still be
operating on the HOST filesystem while the task lives in the container. An
*installed* agent runs inside the environment, which is where bough's host
functions already expect to be. So bough is installed into the task image and
driven headlessly by the CLI it already has.

WHAT HARBOR OWNS AND WHAT WE OWN. Harbor owns the container lifecycle, the
network policy, the verifier and the reward — everything that made reusing this
bank look expensive. We own exactly two things: getting bough into the image, and
running one turn. The AHE trace layer rides along by pointing `BOUGH_TRACE_DIR`
at a directory inside the container and pulling it out afterwards, so a harbor
run produces the same per-round evidence `ahe/materialize.ts` already reads.

THE ONE THING THAT WILL BITE. Many Terminal-Bench tasks declare
`network_mode = "no-network"`, and bough's own API calls go out over that same
network. Harbor applies network policy per phase, so the agent phase needs the
provider host allowlisted (`--allow-agent-host`) even when the task's baseline is
closed. A task whose agent phase cannot reach the API does not fail as "the model
was wrong" — it fails as a connection error, and the ERROR_PATTERNS inherited
from BaseInstalledAgent will classify it as such rather than scoring it as a
capability result.
"""

import json
import os
import shlex
from typing import override

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

# Where bough is installed and where it keeps its state, inside the container.
BOUGH_DIR = "/opt/bough"
BOUGH_HOME = "/opt/bough-home"
TRACE_DIR = "/opt/bough-trace"
PORT = "4321"
MODEL_DEFAULT = "openai/gpt-5.6-luna"


class Bough(BaseInstalledAgent):
    """Runs one headless bough turn against the task's working directory."""

    # Harbor calls populate_context_post_run even when run() raised — a timeout, a
    # container that died, an API error. Without a class-level default that reads
    # as AttributeError and takes down the WHOLE job: one bad trial killed an
    # 89-task run at task 4. A failed trial must cost one trial.
    _stdout: str = ""

    @staticmethod
    @override
    def name() -> str:
        return "bough"

    @override
    def version(self) -> str | None:
        return self._version

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        # Baked image? Nothing to do. Checking rather than assuming keeps one
        # adapter working for both paths.
        probe = await environment.exec(f"test -d {BOUGH_DIR}/node_modules")
        if probe.return_code == 0:
            self.logger.debug("bough is already baked into this image")
            return

        # ONE FILE, NO NETWORK. The first version of this uploaded src/ as a file
        # tree and then ran `bun install` inside every container. Across 89 tasks at
        # 4-way concurrency that cost ~2 minutes a trial and timed out 12 of them in
        # agent setup — 12 tasks scored zero for a task bough was never given. A
        # prebuilt bundle (src + node_modules, ~22MB) is a single copy and an
        # extract: no per-file compose cp, no package registry, nothing that can be
        # slow because someone else's mirror is slow.
        #
        # The bundle is built by ahe/harbor/bundle.sh from the working tree, so the
        # harness under test is still this checkout and not a release.
        bundle = os.environ.get("BOUGH_BUNDLE", "/tmp/bough-bundle.tgz")
        if not os.path.exists(bundle):
            raise RuntimeError(
                f"{bundle} is missing — run ahe/harbor/bundle.sh first. Building it "
                "per trial would put the cost back where it was."
            )
        await self.exec_as_root(
            environment,
            command=f"mkdir -p {BOUGH_DIR} {BOUGH_HOME} {TRACE_DIR} && "
                    f"chmod 777 {BOUGH_HOME} {TRACE_DIR}",
        )
        await environment.upload_file(
            source_path=bundle, target_path=f"{BOUGH_DIR}/bundle.tgz"
        )
        # `bun` is the only thing that still has to come from outside, and only when
        # the image has none. Kept last so a slow install cannot delay the upload.
        await self.exec_as_root(
            environment,
            command=(
                # tar's exit status is deliberately NOT trusted here: it reports 1
                # for warnings it emits about headers it does not recognise, and the
                # explicit test below is what actually establishes the install.
                f"set -u; cd {BOUGH_DIR}; tar xzf bundle.tgz 2>/dev/null || true; "
                f"rm -f bundle.tgz; "
                # The interpreter rides in the bundle, so this needs no network and
                # no package manager: an image with neither curl nor unzip used to
                # fail here with "unzip is required to install bun" — a task scored
                # zero over a missing archiver.
                f"mkdir -p /usr/local/bin && "
                # Pick the libc the image actually has. Probing for the musl loader
                # rather than parsing `ldd --version`, which alpine's busybox does
                # not implement the same way.
                f'if ls /lib/ld-musl-* >/dev/null 2>&1; then '
                f'  cp {BOUGH_DIR}/.bough-bun-linux-musl /usr/local/bin/bun; '
                f'else cp {BOUGH_DIR}/.bough-bun-linux /usr/local/bin/bun; fi && '
                f"chmod 0755 /usr/local/bin/bun"
            ),
        )
        # Prove the install landed. Without this the next failure surfaces much
        # later as "bough server did not start", which reads like a bough bug.
        await self.exec_as_root(
            environment,
            command=(
                f"test -f {BOUGH_DIR}/src/server/main.ts && "
                f"test -d {BOUGH_DIR}/node_modules && "
                # Run it, do not just look for it: the previous version checked the
                # files and still died later at "nohup: failed to run command 'bun'",
                # which reads as a bough failure rather than an install that put the
                # interpreter somewhere unusable.
                f"bun --version"
            ),
        )

    @with_prompt_template
    @override
    async def run(
        self, instruction: str, environment: BaseEnvironment, context: AgentContext
    ) -> None:
        env = {
            "ANTHROPIC_API_KEY": self._get_env("ANTHROPIC_API_KEY") or "",
            "OPENAI_API_KEY": self._get_env("OPENAI_API_KEY") or "",
            "OPENROUTER_API_KEY": self._get_env("OPENROUTER_API_KEY") or "",
            "BOUGH_HOME": BOUGH_HOME,
            "BOUGH_PORT": PORT,
            # The AHE trace layer, inert everywhere else, on for every harbor run:
            # a rollout whose per-round request was not recorded cannot be analyzed
            # later, and there is no second chance to record it.
            "BOUGH_TRACE_DIR": TRACE_DIR,
        }
        env = {k: v for k, v in env.items() if v}

        # `bough exec` talks to a server; the server is not a daemon the image
        # ships, so this starts one and waits for it to answer. Polling the real
        # endpoint rather than sleeping: a fixed sleep is either a wasted 10s on
        # every trial or a flake on a slow image, and usually both.
        await self.exec_as_root(
            environment,
            command=(
                f"cd {BOUGH_DIR} && (nohup bun src/server/main.ts >{BOUGH_HOME}/server.log 2>&1 &) && "
                f"for i in $(seq 1 60); do "
                f"  curl -sf http://127.0.0.1:{PORT}/sessions >/dev/null && exit 0; sleep 1; "
                f"done; echo 'bough server did not start' >&2; cat {BOUGH_HOME}/server.log >&2; exit 1"
            ),
            env=env,
        )

        # Pass the id through UNCHANGED. bough routes by the id itself — a bare
        # name goes to Anthropic, `vendor/model` to OpenRouter, `openai:` to OpenAI
        # (llm/client.ts providerFor) — so stripping the prefix the way most
        # adapters do sent `openai/gpt-5.6-luna` to Anthropic as `gpt-5.6-luna`
        # and every trial died on a 404.
        model = self.model_name or MODEL_DEFAULT
        workdir = self._get_env("BOUGH_WORKDIR") or "/app"
        result = await self.exec_as_agent(
            environment,
            command=(
                f"cd {BOUGH_DIR} && bun src/cli/exec.ts -w {shlex.quote(workdir)} "
                f"-m {shlex.quote(model)} --json {self.build_cli_flags()} "
                f"-- {shlex.quote(instruction)} | tee {BOUGH_HOME}/envelope.json"
            ),
            env=env,
        )
        self._stdout = result.stdout or ""

        # Pull the evidence out before the container goes away. The envelope is
        # the outcome; the trace is why. Best-effort: a trial that produced a
        # result must not be turned into a crashed job by a missing log directory.
        try:
            await environment.download_dir(
                source_dir=TRACE_DIR, target_dir=self.logs_dir / "trace"
            )
        except Exception as exc:
            self.logger.warning(f"could not download the bough trace: {exc}")
        (self.logs_dir / "agent-stdout.txt").write_text(self._stdout)

    @override
    def populate_context_post_run(self, context: AgentContext) -> None:
        # `bough exec --json` already reports exactly what AgentContext wants, so
        # this is a field rename rather than a trajectory parse.
        line = next(
            (l for l in reversed(self._stdout.splitlines()) if l.startswith("{")), None
        )
        if not line:
            return
        try:
            envelope = json.loads(line)
        except json.JSONDecodeError:
            return
        usage = envelope.get("treeUsage") or envelope.get("usage") or {}
        context.n_input_tokens = usage.get("inputTokens")
        context.n_output_tokens = usage.get("outputTokens")
        context.n_cache_tokens = usage.get("cacheReadTokens")
        context.cost_usd = usage.get("costUsd")
        context.metadata = {
            "session": envelope.get("session"),
            "status": envelope.get("status"),
        }


# ---------------------------------------------------------------------------
# Running this locally — three environment facts, each of which cost a debug
# cycle and none of which produce a useful error message.
#
# 1. RUN FROM UNDER $HOME. colima mounts only /Users/<you> into the VM. Harbor
#    bind-mounts the trial's log directory into the container, so a job started
#    from /tmp writes its reward file into a path the VM cannot see, and the run
#    fails with RewardFileNotFoundError — which reads exactly like a broken
#    verifier. The oracle agent failing is the tell: it cannot be wrong.
#
# 2. `docker buildx` must exist. Harbor builds task images through BuildKit;
#    without the plugin the build silently no-ops and every trial fails at the
#    same place. `brew install docker-buildx` and link it into
#    ~/.docker/cli-plugins/.
#
# 3. Watch the VM disk. Terminal-Bench images are ~1GB each and colima's default
#    disk fills fast. A full /var/lib/docker shows up as
#    "apt-get update: At least one invalid signature was encountered" inside the
#    container — a GPG error for what is actually ENOSPC. `colima ssh -- df -h
#    /var/lib/docker` before believing anything else; grow it with
#    `colima stop && colima start --disk 120` rather than pruning the images.
#
# Smoke test:
#   cd ~/hb && PYTHONPATH=$HOME/hb harbor run -p smoke -a bough_agent:Bough \
#     -m claude-haiku-4-5
#
# VERIFIED end to end on 2026-07-30. The smoke task returns reward 1.0, and a real
# Terminal-Bench 2 task (gpt2-codegolf) runs clean with 0 exceptions — bough
# finishes, harbor's verifier scores it 0.0, and `agent_result` carries the token
# and cost accounting the envelope reported. The per-turn trace and prompt-section
# manifest come back out of the container intact, so AHE's evidence layer works
# inside someone else's sandbox.
#
# COST OF THE INSTALL PATH. Uploading src/ and running `bun install` per trial adds
# ~2 minutes to every rollout. Fine for a proof, wasteful for a sweep: bake a
# `FROM <task image>` layer with bun and bough already in it and have install()
# skip the upload when it is present.
