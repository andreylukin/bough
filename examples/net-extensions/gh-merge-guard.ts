/**
 * Example Claw Patrol extension: only let a session merge a GitHub PR whose head
 * branch that SAME session created — verified two ways:
 *   1. on the wire: we watched this session POST /repos/{o}/{r}/git/refs and
 *      remembered the branch (survives restarts via ctx.state);
 *   2. out of band: we call the gh API ourselves (server-side, with the server's
 *      token — the sandbox never sees it) and check the PR's head ref + state.
 * Anything unverified is HELD for human approval, not denied — the operator can
 * still click Approve in the Network rail.
 *
 * Install: copy into ~/.bough/net/extensions/ and POST /net/extensions/reload
 * (or restart bough). Extensions run with the server's full permissions.
 */

export const name = "gh-merge-guard";

// deno-lint-ignore no-explicit-any
type Req = { host: string; method: string; path: string; body?: any };
// deno-lint-ignore no-explicit-any
type Ctx = any; // GuardCtx (src/net/extensions.ts); untyped so the file is copy-anywhere

const BRANCH_CREATE = /^\/repos\/([^/]+)\/([^/]+)\/git\/refs$/;
const PR_MERGE = /^\/repos\/([^/]+)\/([^/]+)\/pulls\/(\d+)\/merge$/;

function ghToken(): string | undefined {
  return Deno.env.get("GITHUB_TOKEN") ?? Deno.env.get("GH_TOKEN");
}

export async function gate(req: Req, ctx: Ctx) {
  if (req.host !== "api.github.com") return undefined;
  const path = req.path.split("?")[0];

  // 1. Remember branches this session creates (POST /repos/o/r/git/refs).
  const create = req.method === "POST" && path.match(BRANCH_CREATE);
  if (create) {
    try {
      const body = JSON.parse(ctx.bodyText);
      const branch = String(body.ref ?? "").replace(/^refs\/heads\//, "");
      if (branch && ctx.sessionId) {
        ctx.state.set(`branch:${ctx.sessionId}:${create[1]}/${create[2]}:${branch}`, true);
      }
    } catch {
      // unparseable body — nothing to remember
    }
    return undefined; // creation itself is the static rule set's call
  }

  // 2. Gate PR merges (PUT /repos/o/r/pulls/N/merge).
  const merge = req.method === "PUT" && path.match(PR_MERGE);
  if (!merge) return undefined;
  const [, owner, repo, num] = merge;

  // Verify the PR out of band with the server's own credentials.
  const token = ghToken();
  const res = await ctx.fetch(`https://api.github.com/repos/${owner}/${repo}/pulls/${num}`, {
    headers: {
      accept: "application/vnd.github+json",
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
  });
  if (!res.ok) {
    return { verdict: "hold", reason: `gh-merge-guard: could not verify PR #${num} (HTTP ${res.status})` };
  }
  const pr = await res.json();
  const head = pr?.head?.ref;
  if (pr?.state !== "open") {
    return { verdict: "deny", reason: `gh-merge-guard: PR #${num} is not open (${pr?.state})` };
  }
  const created = ctx.sessionId &&
    ctx.state.get(`branch:${ctx.sessionId}:${owner}/${repo}:${head}`) === true;
  if (created) {
    return { verdict: "allow", reason: `gh-merge-guard: PR #${num} merges ${head}, created by this session` };
  }
  return {
    verdict: "hold",
    reason: `gh-merge-guard: PR #${num} merges '${head}', which this session did not create`,
  };
}
