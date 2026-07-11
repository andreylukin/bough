# Identity-based security boundary

By default bough's agent runs **as you**: same macOS user, your login keychain
readable in-sandbox, your `~/.aws`/`~/.kube` feeding host-side credential minting.
Claw Patrol (the MITM egress gate) is the only wall against external mutations.

The identity boundary makes the agent a **distinct principal** that is read-only
everywhere *by construction* — enforced server-side (IAM / RBAC / GitHub), with
Claw Patrol demoted to visibility + escalation. Your own shell keeps full access.

Everything is behind flags and off by default. Nothing here changes single-user
behavior until you run the cutover. Each piece is independently reversible.

## The four mechanisms

| Layer | Mechanism | Enforced by |
|---|---|---|
| Local FS | dedicated non-admin user `bough` + per-repo ACL grants | macOS file ACLs |
| AWS | SSO read-only creds via a local broker (container-credentials protocol) | IAM (`ReadOnlyAccess`) |
| Kubernetes | proxy stamps `Impersonate-User: bough` on every request | API server RBAC (`view`) |
| GitHub | proxy-injected fine-grained read-only PAT; push held for approval | GitHub token scope |

### Phase 1 — dedicated `bough` user

`scripts/agent-user.sh` (run once, with sudo) creates a standard hidden user
`bough`, a shared group `bough-work` (you + `bough`), and a passwordless
`sudo -u bough` rule. The server moves to a **system LaunchDaemon** running as
`bough` (`BOUGH_AGENT_USER=bough` in `~/.bough/env`).

The agent gets access to a repo only when you grant it:

```bash
bough grant ~/repos/myproject     # inherited rwx ACL for bough-work + traverse ACLs
bough revoke ~/repos/myproject
```

Grants are recorded in `~bough/.bough/grants.json` and seed the new-session
directory picker. Keep project dirs outside `~/Documents`/`~/Desktop`/`~/Downloads`
— those are TCC-protected and a background daemon generally can't be granted Full
Disk Access without an app bundle.

### Phase 2 — AWS read-only (SSO, no role authoring)

Assign yourself the AWS-managed **`ReadOnlyAccess`** permission set in Identity
Center (one console action) and add a `[profile bough-ro]` SSO profile to your
`~/.aws/config`. Then:

```bash
bough broker install     # LaunchAgent in YOUR account; serves creds over loopback
```

`scripts/cred-broker.ts` runs in your account (it needs the interactive SSO
cache) and serves read-only credentials at `GET /aws` over the ECS
container-credentials protocol. The server injects
`AWS_CONTAINER_CREDENTIALS_FULL_URI` + `AWS_CONTAINER_AUTHORIZATION_TOKEN` into
the sandbox, so `aws`/terraform/boto3 all get IAM-enforced read-only creds — the
SSO cache never enters the sandbox. An expired SSO session returns `503` with the
exact `aws sso login` command.

### Phase 3 — Kubernetes demotion via impersonation

One-time per cluster (built-in `view` ClusterRole — nothing authored):

```bash
kubectl create clusterrolebinding bough-view --clusterrole=view --user=bough
```

Set `BOUGH_KUBE_IMPERSONATE=bough`. The proxy authenticates upstream with your
admin credential (minted host-side from the broker's `/aws-admin` endpoint) but
stamps `Impersonate-User: bough` on every request — the API server demotes it to
`view`. The sandbox holds neither token; its kubeconfig is auth-stripped
(`exec`, bearer tokens, and client-cert material all removed) so it can't opt out.

Verify: `kubectl auth whoami` → `bough`; `kubectl get pods` OK; `kubectl delete
pod` → Forbidden; `kubectl get secrets` → Forbidden.

### Phase 4 — GitHub split identity

Create a fine-grained **read-only** PAT (contents / metadata / PRs read). Note
fine-grained PATs do **not** work with SAML-SSO orgs. Install the github bundle
with the token env var:

```bash
bough net add github tokenEnv=BOUGH_GITHUB_PAT   # PAT lives in ~bough/.bough/env
```

The proxy injects the PAT on `api.github.com` + `github.com`; the sandbox only
carries a useless sentinel (`GH_TOKEN=__bough_github_pat__`) that fails closed if
the MITM is ever bypassed. Git SSH remotes are rewritten to HTTPS so git always
traverses the gate. `git fetch`/`clone` (`git-upload-pack`) flow frictionlessly;
`git push` (`git-receive-pack`) always holds for approval.

`gh` is a Go binary that ignores the CA env var — trust the MITM CA in the
keychain once (the exact `security add-trusted-cert` command shows in the Network
rail when the CA is untrusted).

### Phase 5 — gate hardening

Claw Patrol is now **on by default** (opt out with `BOUGH_CLAWPATROL=0`). A human
hold no longer parks a socket forever: after ~120s it fails closed with
*"held for approval — approve in the Network rail and retry"*, and the card stays
live. Approving a held or timed-out request can mint a **session-scoped, short-TTL
grant** (the "Allow … for this session" button) so the retried command passes
without re-asking. The sandbox can no longer reach bough's own API port (closing
the self-approval hole), read the MITM CA private key, or — in agent-user mode —
read the login keychain.

## Cutover runbook

Do this once, after building the phases. Order matters so GitHub/AWS access never
breaks mid-migration.

```bash
# 1. create the user, group, sudoers (once, with sudo)
sudo scripts/agent-user.sh bough

# 2. flip on agent-user mode
echo 'BOUGH_AGENT_USER=bough' >> ~/.bough/env

# 3. migrate state + install the LaunchDaemon (copies env, net/, models, kubeconfig)
bough setup --agent-user

# 4. grant your active project dirs
bough grant ~/repos/project-a
bough grant ~/repos/project-b

# 5. AWS: assign yourself ReadOnlyAccess in Identity Center (console),
#    add [profile bough-ro] to ~/.aws/config, then:
bough broker install

# 6. Kubernetes: per cluster
kubectl create clusterrolebinding bough-view --clusterrole=view --user=bough
echo 'BOUGH_KUBE_IMPERSONATE=bough' >> ~/.bough/env   # (added to the AGENT env by setup)

# 7. GitHub: create the fine-grained read-only PAT, put it in ~bough/.bough/env, then:
bough net add github tokenEnv=BOUGH_GITHUB_PAT

# 8. start as the agent user + trust the CA (command shown in the Network rail)
bough start
```

### Rollback

```bash
bough teardown --agent-user   # stop + remove the daemon, drop repo ACLs
bough broker uninstall
# then, as printed by teardown:
sudo rm -f /etc/sudoers.d/bough-agent-user /etc/sudoers.d/bough-agent
sudo sysadminctl -deleteUser bough
# remove BOUGH_AGENT_USER from ~/.bough/env, then: bough start   (back to single-user)
```

Every phase is also individually reversible by unsetting its flag
(`BOUGH_AGENT_USER`, the broker vars, `BOUGH_KUBE_IMPERSONATE`, `BOUGH_CLAWPATROL`).
