"""A Modal environment that bakes bough's dependencies into the task image.

Harbor's own Modal backend starts every sandbox from the task's pinned
`docker_image` and leaves the agent to install itself at run time. For bough
that means an `apt-get` and a 26MB binary upload per trial: ~76s on Modal,
~241s locally at n=12, times 445 trials, and it was the single largest source
of lost trials before Harbor's setup cap was raised.

Modal images are content-addressed and cached, so the same work done as an
image layer is paid once per task image and is free for every sandbox after
it — including the other four trials of a `-k 5` run. Setup drops to the
sandbox start itself.

Use it with:

    --env bench.harbor.modal_prebaked:PrebakedModal --ak binary=<path>

The binary is taken from BOUGH_BINARY (the launcher exports it) so that the
image layer and the agent's `--ak binary=` stay the same file.
"""

import os
from pathlib import Path
from typing import override

from harbor.environments.modal import ModalEnvironment
import harbor.environments.modal as _modal_module

# Kept in step with the adapter's install(). `rg` and `ast-grep` are named
# unconditionally by bough's system prompt and node is what it runs the
# programs it writes under, so a missing one is a silently worse agent, not a
# louder failure.
PACKAGES = "ca-certificates curl git ripgrep nodejs xz-utils unzip"
BINARY_PATH = "/installed-agent/bough"

# Web search for the agent. Terminal-Bench allows internet access; only the
# benchmark's own site and repo are off limits (reward hacking), which is a
# matter for what the agent is told to search, not for whether the tool is
# here. Baked into the layer so it costs nothing per trial.
BENCHMARK_HOSTS_BLOCKED = (
    'for h in tbench.ai www.tbench.ai github.com www.github.com api.github.com raw.githubusercontent.com codeload.github.com objects.githubusercontent.com huggingface.co hf.co cdn-lfs.huggingface.co; do   printf \'127.0.0.1 %s\\\\n::1 %s\\\\n\' "$h" "$h" >> /etc/hosts; done'
)

PARALLEL_CLI = (
    'if ! command -v parallel-cli >/dev/null 2>&1; then   mkdir -p /root/.local/share /root/.local/bin   && curl -fsSL https://parallel.ai/install.sh | bash   && cp -a /root/.local/bin/parallel-cli /usr/local/bin/parallel-cli   && chmod 0755 /usr/local/bin/parallel-cli; fi'
)

NODE_FIX = (
    # Downloaded then extracted, NOT piped into tar: the pipe fails
    # silently on Debian 11 and leaves node 12 in place, which looks exactly
    # like the bug this is here to fix.
    'if ! node -e \'null ?? 0\' >/dev/null 2>&1; then   case "$(uname -m)" in     x86_64) NA=x64 ;; aarch64|arm64) NA=arm64 ;; *) NA= ;;   esac;   if [ -n "$NA" ]; then     curl -fsSL -o /tmp/node.tar.xz       "https://nodejs.org/dist/v22.11.0/node-v22.11.0-linux-$NA.tar.xz"     && tar -xJ -C /usr/local --strip-components=1 -f /tmp/node.tar.xz;     rm -f /tmp/node.tar.xz;   fi; fi'
)


class PrebakedModal(ModalEnvironment):
    @override
    async def start(self, force_build: bool) -> None:
        original = _modal_module.Image.from_registry

        def from_registry_prebaked(ref, **kwargs):
            image = original(ref, **kwargs)
            # `|| true`: not every task image is Debian, and a base that
            # cannot apt-get must degrade to the adapter installing at run
            # time rather than failing the whole image build. The adapter
            # probes for the binaries and only shells out to apt if they are
            # actually missing.
            image = image.run_commands(
                "apt-get update -qq "
                f"&& DEBIAN_FRONTEND=noninteractive apt-get install -y "
                f"--no-install-recommends {PACKAGES} || true"
            )
            binary = os.environ.get("BOUGH_BINARY")
            if binary and Path(binary).is_file():
                # copy=True so the file lands in the layer itself; without it
                # Modal mounts it at run time and the caching is lost.
                image = image.add_local_file(binary, BINARY_PATH, copy=True)
                image = image.run_commands(
                    f"chmod 0755 {BINARY_PATH} "
                    f"&& ln -sf {BINARY_PATH} /usr/local/bin/bough"
                )
            return image

        _modal_module.Image.from_registry = from_registry_prebaked
        try:
            return await super().start(force_build)
        finally:
            _modal_module.Image.from_registry = original
