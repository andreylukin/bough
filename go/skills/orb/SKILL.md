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

## 3. setup.sh vs resume.sh

- `setup.sh` runs at image build and gets no secrets. Put toolchains and
  system packages there.
- `resume.sh` runs once per session and has secrets available. Put
  dependency installs (`uv sync`, `npm ci`) and devpi or index config there.
- Never echo a secret. resume.log is on the host and bough does not scrub it.

## 4. Caches and env

- Package caches live under `caches:` in project.yml; `set` has no key
  for them. Read `bough project show <slug> project.yml`, edit the text,
  and pipe it back through `bough project write <slug> project.yml`.
- Plain settings: `bough project set <slug> env.NAME value`.

## 5. Secrets

For each credential the repo expects (Makefile `ensure-*` targets,
`.env.example`):

- `tools.secret(NAME, reason)` asks the user, stores the value in the
  keychain and adds the ref to project.yml. You never see the value.
- If the user already has a keychain item:
  `bough project set <slug> secrets.NAME keychain:<service>`.

## 6. Checks

`bough project set <slug> checks.fast "<cmd>"` and `checks.full`. Use the
commands CI runs.

## 7. Write the scripts

`bough project write <slug> setup.sh` or `resume.sh`, with the text on stdin.

## 8. Verify

Changes to the image or setup.sh apply in the next session. Now, run
resume.sh and checks.fast by hand through tools.bash. Report what passed,
what is still unverified, and why.
