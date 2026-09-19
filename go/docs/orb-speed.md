# Orb speed: cached builds, shared caches, many sessions per project

Contract for making heavy project orbs (reference: a large Python + Rust service) fast to
build, cheap to rebuild, and cheap to open N times at once. Companion to
docs/orbs.md; where they disagree, this file is the target and orbs.md is
updated to match.

Measured baseline (an Apple Silicon Mac, `container` 1.4.1, builder-shim 0.13.1):
full build ~7 min (setup.sh RUN 348 s + export 57 s); a hash change with
byte-identical setup.sh already rebuilds in 10 s (BuildKit layer cache hit,
8.6 s export). Every new session reinstalls requirements.txt from scratch
(no uv/cargo cache persisted, `CARGO_BUILD_JOBS=1`).

## 1. Image hash: build inputs only

`projectdef.ImageHash(home, p)` hashes exactly:

| input | setup.sh path | Dockerfile path |
|---|---|---|
| base (`Def.Base` or `BaseTag()`) | yes | yes |
| setup.sh bytes | yes | only if the Dockerfile dir contains it |
| lockfiles a step declares (`# bough:uses`, §2) at the repo's base ref | yes | no |
| project dir files | no | every regular file EXCEPT `project.yml`, `resume.sh`, `MEMORY.md` |

NOT hashed (never rebuild): `checks`, `env`, `secrets`, `identity`, `cpus`,
`memory`, `caches`, `lsp`, `repos` (other than via declared lockfiles),
resume.sh, `MEMORY.md`, and lockfiles no step declares.

Consequences, all required for the hash change to be honest:
- The generated Dockerfile emits no `ENV` lines. `Def.Env` reaches the
  container through run env + exec env (already the case: `baseEnv`,
  `execEnv`). A setup.sh that needs a variable sets it itself.
- `withoutSecrets` goes away; project.yml is simply not a hash input on
  either path (only `Def.Base` is read from it).
- Changing `cpus`/`memory`/`caches` affects only newly created containers.
  An existing container is recreated only when its image tag differs (as
  today) or its run shape differs (resources/mounts, compared on Inspect).

One-time cost: the new hash differs from the old one for EVERY existing
project, edited or not (project.yml and undeclared lockfiles are no
longer inputs), so each project's next session after upgrading does one
full build (~7 min for the reference project; BuildKit's layer cache does not
help, because the generated Dockerfile changed shape). Measured on the
test machine: an unchanged project went from `e857a920c1c3` to
`ba230460a93c`.

Dockerfile path: project.yml and resume.sh ARE hashed when the Dockerfile
text names them (a `COPY resume.sh` bakes it in). MEMORY.md never is, on
either path and whatever the Dockerfile says — a `COPY . .`, or the mere
mention of it in a comment, would otherwise retag the image every time
the brief was edited, and a new tag makes `orb.Open` remove and recreate
the container. The guest reads it from the definition directory, which is
mounted, not from a layer.

Pruning: a build removes old tags no container uses. A session that got
its tag but has not started its container yet can lose it; Open then
re-runs EnsureImage once and starts on the result.

What rebuilds, exhaustively: editing setup.sh (from the first changed step
onward, §2), changing `base:`, a base.Dockerfile change in bough (new
`BaseTag`), a declared lockfile changing at the base ref, or any file other
than project.yml/resume.sh in a Dockerfile project dir.

## 2. Layered builds

Apple `container build` has no `--cache-from/--cache-to`; the only cache is
BuildKit's local layer cache inside the long-lived `buildkit` builder
container (lost if the builder is recreated — then one full build). That
cache is per-layer, so bough emits one layer per logical step.

setup.sh is split on step markers (plain comments, script stays runnable as
a whole with `bash setup.sh`):

```bash
#!/usr/bin/env bash
set -euo pipefail
# bough:step apt
apt-get update && apt-get install -y ...
# bough:step rust
curl ... | sh -s -- -y --profile minimal --default-toolchain 1.96.0
# bough:step python-deps
# bough:uses requirements.txt
uv pip install --system -r /bough-setup/lock/app/requirements.txt
```

Text before the first marker (shebang, `set -e...`) is the preamble and is
prepended to every step. No markers = one step (today's behaviour).

Generated Dockerfile (`container.CommitSpec.Steps`):

```
FROM <base>
COPY steps/01-apt.sh /bough-setup/steps/
RUN <argv> /bough-setup/steps/01-apt.sh
COPY steps/02-rust.sh /bough-setup/steps/
RUN <argv> /bough-setup/steps/02-rust.sh
COPY steps/03-python-deps.sh lock/<repo>/requirements.txt /bough-setup/...
RUN <argv> /bough-setup/steps/03-python-deps.sh
```

Each step's script and its declared lockfiles are COPYed immediately before
its RUN, so an edit to step k (or a bump of a file only step k uses)
re-executes steps k..n and reuses 1..k-1. Lockfiles are no longer copied
wholesale. `ScriptArgv` applies to the preamble shebang.

The Dockerfile path is unchanged: authors order RUN layers themselves; the
SKILL tells them to put apt first and deps last.

Must be verified live before relying on it (builder-shim 0.13.1), with a
throwaway tag, never touching serve:
1. two-step file, edit step 2 → step 1 reported `CACHED`;
2. `RUN --mount=type=cache,target=/root/.cache/uv` accepted and reused;
3. `RUN --mount=type=secret,id=X,env=X` + `container build --secret id=X,env=X`
   sees the value and `container image inspect`/history shows no trace.
If 2 or 3 fail, the features that use them (below) stay off; §2 layering
alone does not depend on them.

## 3. Many sessions per project

Build serialisation (`orb.EnsureImage`), extended:
- One flock per project: `~/.bough/orbs/images/<slug>/build.lock` (exists).
  Serve's `POST /api/projects/{slug}/orb/build` takes the same lock (exists).
- After acquiring the lock the waiter RE-HASHES (definition may have been
  edited while it waited) and re-checks `ImageExists`; it builds only if
  the fresh tag is still missing. N sessions opened during a build → one
  build, N-1 cache hits.
- Waiters show `building` and tail `build.log` into their own open log, so a
  waiting session is not silent.
- Bound: plugin Open timeout stays 30 min (plugins/orb/orb.go), > 7 min build.
- Old tags are pruned after a successful build: remove
  `bough-orb/<slug>:*` tags not in use by any existing container
  (a busy project had accumulated 7 tags).

Per-session isolation (unchanged, restated): each session has its own
container, rootfs (venv, postgres data, rabbitmq state), worktrees and
scratch. The project dir is mounted read-only. Only cache volumes are
shared.

## 4. Shared dependency caches

`caches:` entries are guest dirs backed by per-project host directory
binds (virtiofs): `~/.bough/cache/<slug>/<sha256(guestpath)[:10]>`,
created by bough. Named by path, not index, so reordering never remaps a
dir.

Decided: NOT named volumes. An Apple container volume is a block-backed
ext4 image; the live run found a second concurrent session of a project
with a volume fails to boot (VZErrorDomain Code=2), and two kernels
mounting one ext4 read-write would corrupt it anyway. Old
`bough-cache-*` volumes are left alone. Build-time
`RUN --mount=type=cache` is not used.

Tool-level safety given a safely shareable dir:
- uv: cache is lock-protected and content-addressed; safe to share.
  Set `UV_CACHE_DIR=/root/.cache/uv` and `UV_LINK_MODE=copy` (venv on
  rootfs, cache on another fs: hardlinks fail, uv warns and falls back).
- cargo: NOT shared. Its package-cache lock is `$CARGO_HOME/.package-cache`,
  a flock, and the live check found flock does not cross containers on
  virtiofs (a second container took the lock while the first held it), so
  two sessions sharing CARGO_HOME or `registry/` race extraction. Each
  container keeps its own CARGO_HOME; the toolchain stays in the image.
- pip: `PIP_CACHE_DIR=/root/.cache/pip` if used; safe.
- NEVER share: venvs, `target/`, database data dirs, node_modules.

## 5. Where dependency install happens; secrets

Rule: install into the image when the index is public (deterministic,
hashed by the declared lockfile, zero per-session cost). Install per
session in resume.sh against a shared cache when credentials are needed.

Secrets never enter a layer. Build-time secrets (`--secret` +
`RUN --mount=type=secret`) are NOT adopted now: a private Cargo registry
token can be minted per session by a host cloud CLI, private package index
values are per-exec env, and baking private wheels would tie image
validity to credential lifetimes. Revisit only if §2 check 3 passes AND a
project has long-lived build credentials.

So for such a project: resume.sh keeps `uv pip install -r requirements.txt`
into a per-container venv, but with the uv + cargo caches shared, a second
session's install is a cache hit (downloads and built wheels reused, no
Rust wheel recompiles). Public tooling (apt, rust, uv python, circleci)
stays in layered image steps.

## 6. Resume execution

No semantic change: resume.sh runs per new/restarted container, with exec
env + secrets, output to `<orbdir>/resume.log`. Added: resume.log records
start/end timestamps and duration so starts can be measured without
parsing agent output.

## 7. Tests (fake runtime, `container.Fake`)

projectdef:
- hash unchanged when editing checks/env/secrets/identity/cpus/memory/caches/resume.sh;
- hash changes on setup.sh, base, declared lockfile; unchanged on undeclared lockfile;
- Dockerfile path ignores project.yml/resume.sh.
- step splitting: no markers = one step; preamble prepended; `# bough:uses` parsed; unknown file → validation error.
container:
- generated Dockerfile: no ENV; COPY-before-RUN per step; only declared lockfiles copied.
orb:
- N concurrent `EnsureImage` on the same hash → exactly one Build/Commit call;
- definition edited while waiting → waiter builds the new hash once, not the stale one;
- cache volume names stable under reordering; same name across two sessions;
- prune removes unused tags only.
plugins/orb: skill_test.go assertions updated for new SKILL.md wording.

## 8. Live measurement plan

Never stop/restart the serve on 127.0.0.1:7684, never touch its sessions or
existing containers. Use a binary built from the change.

1. Builder checks (§2 1-3, §4 concurrency) with throwaway tags/volumes named
   `bough-speedtest-*`; remove only those afterwards.
2. From a temp cwd (`mktemp -d`), time `bough --headless --project
   example-app "reply ok"` → cold build (after the definition change),
   first session start (build end → resume.sh end, from build.json +
   resume.log).
3. Edit the last setup.sh step trivially; rerun → expect only that step
   re-executes (target: < 1 min incl. export).
4. Launch two headless runs simultaneously from two temp cwds → expect one
   build; record each session's resume duration. Target: second session
   resume in seconds-to-low-minutes (cache hit), versus the current
   full install.
5. Close the headless runs; remove only the containers those session ids
   created.

## Status

Area A is implemented against the fake runtime (§7 tests). Not yet done:
the §2/§4 live builder checks, `RUN --mount=type=cache` (off until check 2
passes), the run-shape comparison on Inspect (§1: a cpus/memory/caches
change still only reaches new containers), and a log for waiters opened
from a session (Open passes no log to EnsureImage, so only callers that
pass one tail build.log).

## File split

Area A — bough code (repo, go/):
- internal/projectdef/hash.go, projectdef.go, projectdef_test.go (hash inputs, step parsing/validation)
- internal/container/runtime.go, apple.go, linux.go, fake.go, container_test.go (CommitSpec.Steps, no ENV, per-step COPY/RUN)
- internal/orb/image.go, orb.go, orb_test.go (re-hash under lock, waiter log tail, prune, cache dir naming + env, resume timing)
- plugins/orb/SKILL.md, plugins/orb/skill_test.go
- docs/orbs.md, docs/orb-speed.md

Area B — the reference project's definition on a test machine (only via `bough project` there,
after Area A is built there; drafts in a scratch dir first):
- ~/.bough/projects/example-app/setup.sh: step markers (apt, uv-python, rust, circleci)
- ~/.bough/projects/example-app/project.yml: `caches: [/root/.cache/uv]`, env `UV_CACHE_DIR`, `UV_LINK_MODE=copy` (no shared CARGO_HOME, see §4)
- ~/.bough/projects/example-app/resume.sh: keep install per venv; drop nothing that needs secrets; rely on caches
