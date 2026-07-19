---
name: cloud
description: Explore and wire up AWS / Kubernetes access for this session — probe what works, diagnose what's missing, guide the operator through the rest
---

# Cloud access: explore, diagnose, wire, verify

Figure out what AWS and Kubernetes access this session has, what the operator's
machine could provide, and close the gap. You are sandboxed: `~/.aws`,
`~/.kube`, and `~/.bough/env` are read-denied BY DESIGN, and you cannot install
host daemons. A "permission denied" on those paths is the sandbox working, not
a bug — the operator's credentials are supposed to be invisible to you. Your
job is to probe what IS visible, infer the rest, and hand the operator exact
commands for the few steps only they can do.

How access flows when fully wired (all method-agnostic — SSO, static keys,
assume-role, any kubeconfig exec plugin):

- **AWS**: a host-side broker (LaunchAgent, port 9109) resolves the operator's
  own credential chain and vends short-lived creds over the ECS
  container-credentials protocol. Every session child gets
  `AWS_CONTAINER_CREDENTIALS_FULL_URI` + bearer injected automatically — every
  AWS SDK and the CLI honor it with zero per-tool config.
- **Kubernetes**: the server rewrites the operator's kubeconfig (cluster CAs →
  bough's proxy CA, all auth stripped) into the session, runs the kubeconfig's
  exec plugin host-side, and stamps the short-lived bearer onto cluster
  requests at the proxy.

## 1. Probe what this session already has

```bash
command -v aws kubectl; env | grep -E 'AWS_CONTAINER|KUBECONFIG'
aws sts get-caller-identity          # works → AWS is fully wired; report the identity
kubectl auth whoami 2>&1 | head -3   # works → kube is fully wired
```

Interpret:
- `AWS_CONTAINER_CREDENTIALS_FULL_URI` set but sts fails with 503-ish error →
  broker up, operator's SSO session expired → they run
  `aws sso login --profile <name>` (the broker's error text names the profile).
- Env vars absent → broker not wired (go to step 2).
- `KUBECONFIG` absent → no kubeconfig existed when the server started (step 3).
- CLI missing → `brew install awscli` / `brew install kubernetes-cli`
  (operator runs this; confirm afterward by re-probing `command -v`).

## 2. AWS not wired — explore, then guide

Probe the broker directly (loopback is reachable from the sandbox):

```bash
TOK=$(cat /Users/Shared/.bough-broker-token 2>/dev/null || cat ~/.bough/broker-token 2>/dev/null)
curl -s -m 3 -H "authorization: Bearer $TOK" http://127.0.0.1:9109/aws | head -c 200
```

- Connection refused → broker not running/installed.
- 200 with creds → broker fine; only the env wiring or a server restart is
  missing.
- 503 → operator needs `aws sso login` (the body says the exact command).

You cannot see the operator's profiles (`aws configure list-profiles` will be
denied reading `~/.aws`). Ask the operator which profile the agent should get —
ideally a read-only one — and note that `!` commands they run locally are
invisible to you, so ask them to paste the profile list if they're unsure:

> `! aws configure list-profiles`  — then paste the output

Then have them run (the `!` prefix runs it locally, outside the sandbox):

> `! bash scripts/bough broker install --profile <NAME>`
> `! bash scripts/bough restart`

(`broker install` preflights the profile, installs the LaunchAgent, wires the
server env, and verifies a live mint. If they have no profiles at all:
`aws configure sso` or `aws configure` first.)

## 3. Kubernetes not wired — explore, then guide

The session kubeconfig only exists if the operator had one at server start.
Ask what cluster access they use day-to-day; typical wiring:

> `! aws eks update-kubeconfig --name <cluster> --profile <NAME>`   (EKS)
> `! bash scripts/bough restart`

Any provider works — whatever exec plugin or token their kubeconfig uses runs
host-side. Client-certificate auth is the one unsupported shape (can't be
stamped at the proxy); flag it if kubectl errors suggest cert auth.

## 4. Verify and report

After each operator step, re-probe from IN HERE (step 1 commands) — that is the
proof, not their local output. Finish with a short report: the AWS identity
(account + arn from sts), the kube user/contexts, anything still broken, and a
reminder that creds are short-lived and scoped to the profile they chose —
recommend a read-only profile if they wired a privileged one.
