---
name: orb
description: "Set up or repair a project session's definition (image, setup.sh, resume.sh, env, secrets, checks) from what the repos expect. Use as /orb <slug>."
manual: true
---

# orb

Install once: `ln -s "$PWD/go/skills/orb" ~/.bough/skills/orb` (from the
bough checkout).

Goal: a project whose checks really run. Never settle for weaker
verification because a credential, dependency or tool is missing.

## 1. Inspect

- `bough project show <slug>`.
- In each repo, read the Makefile, docker-compose, CI config
  (`.github/workflows`, `.circleci`), README, lockfiles and `.tool-versions`
  with tools.view or tools.bash.

## 2. Base image, setup.sh or Dockerfile

Use the base image plus `setup.sh` when the toolchain installs with apt or
curl. Write a Dockerfile only when the repo needs a different distro.

Split setup.sh into cached layers with step markers, slowest-changing
first (apt, then toolchains, then dependencies):

```bash
#!/usr/bin/env bash
set -euo pipefail
# bough:step apt
apt-get update && apt-get install -y ...
# bough:step node-deps
# bough:uses package-lock.json
cd /bough-setup/lock/<repo> && npm ci
```

- Lines before the first `# bough:step` run at the top of every step.
- Editing a step rebuilds that step and the ones after it.
- `# bough:uses <file>` copies that repo file, at the base branch, to
  `/bough-setup/lock/<repo>/<file>`. A new commit to the file rebuilds
  its step. Undeclared repo files are not in the build.
- setup.sh gets no project env. Export what it needs inside the script.
- In a Dockerfile, order RUN lines the same way: apt first, deps last.
- Only setup.sh, `base:`, declared files and a Dockerfile dir rebuild
  the image. Env, secrets, checks, caches, cpus, memory and resume.sh
  do not.

## 3. setup.sh vs resume.sh

- `setup.sh` runs at image build and gets no secrets. Put toolchains and
  system packages there.
- Lockfile-pinned dependencies from a public index go in a setup.sh step
  with `# bough:uses`. setup.sh gets no build-time secrets or
  `RUN --mount=type=cache`.
- `resume.sh` runs once per session and has secrets available. Put
  dependency installs that need credentials (devpi, CodeArtifact) and
  index config there.
- Never echo a secret. resume.log is on the host and bough does not scrub it.

## 4. Caches and env

- Package caches live under `caches:` in project.yml; `set` has no key
  for them. Read `bough project show <slug> project.yml`, edit the text,
  and pipe it back through `bough project write <slug> project.yml`.
- Every session of the project shares each cache dir at once. Cache only
  tools that lock their own cache (uv, pip, go-build). Never cache a
  venv, `target/`, node_modules, a database dir or CARGO_HOME.
- Plain settings: `bough project set <slug> env.NAME value`.

## 5. Secrets

For each credential the repo expects (Makefile `ensure-*` targets,
`.env.example`):

- `tools.secret(NAME, reason)` asks the user, stores the value in the
  keychain and adds the ref to project.yml. You never see the value.
- If the user already has a keychain item:
  `bough project set <slug> secrets.NAME keychain:<service>`.
- Secret values (8+ chars) show as `[redacted:NAME]` in command output,
  history and resume.log. `redact: false` in project.yml turns that off
  only if the user asks.

## 5b. Host identity

A project container gets none of the user's logins by default: no
`GH_TOKEN`, no `~/.aws`, `~/.kube` or other CLI config. When the repo's
checks or resume.sh need one, ask the user first, then:

- `bough project add-identity <slug> gh` passes the host's `gh auth token`
  as GH_TOKEN and sets git's GitHub credential helper.
- `bough project add-identity <slug> .aws` mounts `~/.aws` read-only at
  `/root/.aws`. Append `:rw` (`.kube:rw`) only for SSO or token caches
  that must refresh in place.
- `.ssh`, `.gnupg` and `.bough` are always refused. Remove one with
  `bough project remove-identity <slug> <entry>`. It applies next session.

## 6. Checks

`bough project set <slug> checks.fast "<cmd>"` and `checks.full`. Use the
commands CI runs.

## 7. Write the scripts

`bough project write <slug> setup.sh` or `resume.sh`, with the text on stdin.

## 8. Verify

Changes to the image or setup.sh apply in the next session. Now, run
resume.sh and checks.fast by hand through tools.bash. Report what passed,
what is still unverified, and why.
