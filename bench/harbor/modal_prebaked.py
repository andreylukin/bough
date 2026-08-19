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
PACKAGES = "ca-certificates curl git ripgrep nodejs libssl3"
BINARY_PATH = "/installed-agent/bough"


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
