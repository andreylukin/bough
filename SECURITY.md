# Security Policy

## Read this first: bough has no isolation boundary

This is a design decision, documented in [`docs/spec.md`](docs/spec.md) §2 and in the
README. Programs the model writes run **as you, with your full authority** — filesystem,
network, subprocesses, arbitrary `npm:` imports. There is no sandbox, no egress proxy, and
no credential gating. Host functions are convenience and session integration, never a
wall.

Therefore the following are **not** vulnerabilities, and reports about them will be closed
as working-as-designed:

- A program reading, writing, or deleting files anywhere the invoking user can.
- A program making arbitrary network requests, or exfiltrating anything readable.
- A program spawning subprocesses, or escaping any host function into a raw shell.
- A model being prompted into doing any of the above.

Run bough only on a machine where you would be comfortable running the code it writes,
because that is exactly what happens.

## What *is* in scope

- **Credential handling.** Tokens for MCP servers and model providers being written with
  loose permissions, logged, echoed into the transcript, persisted where they shouldn't
  be, or sent to a host other than the one they belong to.
- **The loopback server.** `bough-server` binds to loopback and is unauthenticated by
  design for the local user; anything that lets an *off-host* or cross-user party reach
  it, or that widens its binding, is in scope.
- **Session and history integrity.** A path where the recorded transcript, branch tree, or
  replay journal can be made to misrepresent what actually ran.
- **Supply chain.** A compromised or typosquatted dependency, or a build/release step that
  could ship something other than what is in the tree.
- **The updater and `install.sh`.** Anything that lets a third party influence what a user
  installs or upgrades to.

## Reporting

Report privately — **do not open a public issue**.

Use GitHub's private vulnerability reporting: **Security → Report a vulnerability** on
<https://github.com/andreylukin/bough/security/advisories/new>.

Please include the version or commit, your platform, a reproduction, and what an attacker
gains. A working proof of concept helps a lot.

## What to expect

- **Acknowledgement within 3 business days.**
- An assessment — in-scope or not, and a severity — within 10 business days.
- For confirmed in-scope issues: a fix or a documented mitigation, coordinated disclosure
  via a GitHub Security Advisory, and credit in the advisory unless you'd rather not be
  named.

bough is pre-1.0 and unversioned in the usual sense: fixes land on `main`, and only the
latest commit is supported. There are no backports to older tags.
