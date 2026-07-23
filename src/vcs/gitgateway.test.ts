/**
 * Store-gateway tests — host-only, no VM: a plain local `git` client plays the
 * guest against the served store. Pins the smart-HTTP round-trip (fetch incl.
 * alternates-grafted origin history — the exact gap that broke mounted-store
 * clones), the pre-receive session-ref confinement, bearer auth, the
 * receive→mirror→changes.updated chain, and receive/host-writer serialization
 * via withLock.
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { bus } from "../bus.ts";
import * as shadow from "./shadow.ts";
import {
  gitGatewayUrl,
  mintSessionToken,
  revokeSessionToken,
  startGitGateway,
  stopGitGateway,
} from "./gitgateway.ts";
import { pathExists } from "../fsutil.ts";

async function sh(cwd: string, bin: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command(bin, { args, cwd, stdout: "piped", stderr: "piped" }).output();
  if (r.code !== 0) {
    throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(r.stderr)}`);
  }
  return new TextDecoder().decode(r.stdout);
}

async function shFails(cwd: string, bin: string, ...args: string[]): Promise<string> {
  const r = await new Deno.Command(bin, { args, cwd, stdout: "piped", stderr: "piped" }).output();
  if (r.code === 0) throw new Error(`${bin} ${args.join(" ")} unexpectedly succeeded`);
  return new TextDecoder().decode(r.stderr);
}

/** A scratch origin repo with one commit and one uncommitted file. */
async function makeRepo(): Promise<string> {
  const repo = await Deno.makeTempDir({ prefix: "bough-gw-origin-" });
  await sh(repo, "git", "init", "-q", "-b", "main");
  await sh(repo, "git", "config", "user.name", "t");
  await sh(repo, "git", "config", "user.email", "t@t");
  await Deno.writeTextFile(`${repo}/committed.txt`, "committed\n");
  await sh(repo, "git", "add", "-A");
  await sh(repo, "git", "commit", "-q", "-m", "init");
  await Deno.writeTextFile(`${repo}/untracked.txt`, "untracked\n");
  return repo;
}

/** The §2 guest bootstrap, played by a plain host git client. The bearer is
 * stamped URL-SCOPED exactly as stampRemote does in the guest — this pins that
 * git's urlmatch applies the header to the gateway's deeper request paths
 * (`…/info/refs`, `…/git-upload-pack`) while an unrelated remote gets nothing. */
async function bootstrapClone(url: string, sid: string, token: string): Promise<string> {
  const clone = await Deno.makeTempDir({ prefix: "bough-gw-clone-" });
  await sh(clone, "git", "init", "-q", ".");
  await sh(clone, "git", "config", "user.name", "bough");
  await sh(clone, "git", "config", "user.email", "bough@localhost");
  await sh(clone, "git", "config", `http.${url}.extraHeader`, `Authorization: Bearer ${token}`);
  await sh(clone, "git", "remote", "add", "origin", url);
  await sh(
    clone,
    "git",
    "fetch",
    "-q",
    "origin",
    `+refs/bough/sessions/${sid}:refs/remotes/origin/session`,
    `+refs/bough/base/${sid}:refs/bough/base`,
    `+refs/bough/originbase/${sid}:refs/bough/originbase`,
  );
  await sh(clone, "git", "checkout", "-q", "-B", "work", "refs/remotes/origin/session");
  return clone;
}

interface Ctx {
  repo: string;
  sid: string;
  store: string;
  token: string;
  url: string;
  cleanup: () => Promise<void>;
}

/** Temp roots + resolver + a started gateway with a minted token for `sid`. */
async function setup(sid: string): Promise<Ctx> {
  const shadowBase = await Deno.makeTempDir({ prefix: "bough-gw-store-" });
  const wsBase = await Deno.makeTempDir({ prefix: "bough-gw-ws-" });
  Deno.env.set("BOUGH_SHADOW_BASE", shadowBase);
  Deno.env.set("BOUGH_SUBAGENT_BASE", wsBase);
  Deno.env.set("BOUGH_GATE_HOST", "127.0.0.1");
  const repo = await makeRepo();
  shadow.setOriginResolver((id) => (id === sid ? repo : null));
  const store = await shadow.createSessionWorkspace(repo, sid, { worktree: false });
  startGitGateway();
  const token = mintSessionToken(sid);
  return {
    repo,
    sid,
    store,
    token,
    url: gitGatewayUrl(sid),
    cleanup: async () => {
      await stopGitGateway();
      revokeSessionToken(sid);
      shadow.setOriginResolver(() => null);
      Deno.env.delete("BOUGH_SHADOW_BASE");
      Deno.env.delete("BOUGH_SUBAGENT_BASE");
      Deno.env.delete("BOUGH_GATE_HOST");
      for (const d of [shadowBase, wsBase, repo]) {
        await Deno.remove(d, { recursive: true }).catch(() => {});
      }
    },
  };
}

Deno.test("gateway: bootstrap fetch transfers the alternates-grafted origin history", async () => {
  const ctx = await setup("gw-s1");
  try {
    const clone = await bootstrapClone(ctx.url, ctx.sid, ctx.token);
    // Working tree = the captured base (committed + untracked files).
    assertEquals(await Deno.readTextFile(`${clone}/committed.txt`), "committed\n");
    assertEquals(await Deno.readTextFile(`${clone}/untracked.txt`), "untracked\n");
    // The base commit's parent is the ORIGIN's HEAD commit, whose objects live
    // only behind objects/info/alternates — a mounted-store clone cannot see
    // them; the host-side upload-pack must have packed them.
    const originHead = (await sh(ctx.repo, "git", "rev-parse", "HEAD")).trim();
    await sh(clone, "git", "cat-file", "-e", `${originHead}^{commit}`);
    const log = await sh(clone, "git", "log", "--format=%H", "work");
    assertStringIncludes(log, originHead, "grafted origin history reachable from the session tip");
    // originbase arrived too (the ship-note contract: diffable in-guest).
    await sh(clone, "git", "rev-parse", "--verify", "refs/bough/originbase");
    await Deno.remove(clone, { recursive: true }).catch(() => {});
  } finally {
    await ctx.cleanup();
  }
});

Deno.test("gateway: push to the session ref lands; mirror + changes.updated follow", async () => {
  const ctx = await setup("gw-s2");
  const events: Array<{ type: string; sessionId?: string }> = [];
  const unsub = bus.subscribe((e) => events.push(e));
  try {
    const clone = await bootstrapClone(ctx.url, ctx.sid, ctx.token);
    await Deno.writeTextFile(`${clone}/committed.txt`, "guest edit\n");
    await Deno.writeTextFile(`${clone}/guest-new.txt`, "new\n");
    await sh(clone, "git", "add", "-A");
    await sh(clone, "git", "commit", "-q", "-m", "snapshot");
    await sh(clone, "git", "push", "-q", "origin", `HEAD:refs/bough/sessions/${ctx.sid}`);
    // Store tip advanced to the pushed commit.
    const cloneHead = (await sh(clone, "git", "rev-parse", "HEAD")).trim();
    const storeTip = (await sh(ctx.store, "git", "rev-parse", `refs/bough/sessions/${ctx.sid}`))
      .trim();
    assertEquals(storeTip, cloneHead);
    // Mirror refreshed to the pushed tree.
    const mirror = shadow.workspaceDirFor(ctx.sid);
    assertEquals(await Deno.readTextFile(`${mirror}/committed.txt`), "guest edit\n");
    assertEquals(await Deno.readTextFile(`${mirror}/guest-new.txt`), "new\n");
    // Rail woken.
    assert(events.some((e) => e.type === "changes.updated" && e.sessionId === ctx.sid));
    // Store-side diff (dir = the bare store) shows the guest work — the exact
    // read the Changes rail does post-push.
    const d = await shadow.diff(ctx.store, ctx.sid);
    const paths = d.files.map((f) => f.path).sort();
    assertEquals(paths, ["committed.txt", "guest-new.txt"]);
    await Deno.remove(clone, { recursive: true }).catch(() => {});
  } finally {
    unsub();
    await ctx.cleanup();
  }
});

Deno.test("gateway: pre-receive refuses any ref but the session's own", async () => {
  const ctx = await setup("gw-s3");
  try {
    const clone = await bootstrapClone(ctx.url, ctx.sid, ctx.token);
    await Deno.writeTextFile(`${clone}/evil.txt`, "evil\n");
    await sh(clone, "git", "add", "-A");
    await sh(clone, "git", "commit", "-q", "-m", "evil");
    const err = await shFails(clone, "git", "push", "origin", "HEAD:refs/heads/evil");
    assertStringIncludes(err, "refused");
    const refs = await sh(ctx.store, "git", "for-each-ref", "refs/heads");
    assertEquals(refs.trim(), "", "no head ref was created");
    // Another session's ref is equally refused.
    const err2 = await shFails(clone, "git", "push", "origin", "HEAD:refs/bough/sessions/other");
    assertStringIncludes(err2, "refused");
    await Deno.remove(clone, { recursive: true }).catch(() => {});
  } finally {
    await ctx.cleanup();
  }
});

Deno.test("gateway: absent, wrong, and revoked tokens are 401", async () => {
  const ctx = await setup("gw-s4");
  try {
    // Raw probe: no header.
    const bare = await fetch(`${ctx.url}/info/refs?service=git-upload-pack`);
    assertEquals(bare.status, 401);
    await bare.body?.cancel();
    // Wrong token via a real git client: the 401 makes git fall back to a
    // credential prompt, which GIT_TERMINAL_PROMPT=0 turns into a fast failure
    // (the same env the guest needs — plan §7 no-PAT trap).
    const noauthDir = await Deno.makeTempDir({ prefix: "bough-gw-noauth-" });
    const r = await new Deno.Command("git", {
      args: ["-c", "http.extraHeader=Authorization: Bearer wrong", "ls-remote", ctx.url],
      cwd: noauthDir,
      env: { GIT_TERMINAL_PROMPT: "0" },
      stdout: "piped",
      stderr: "piped",
    }).output();
    assert(r.code !== 0, "wrong token must fail ls-remote");
    assertStringIncludes(
      new TextDecoder().decode(r.stderr),
      "could not read Username",
      "401 pushed git into (disabled) credential prompting",
    );
    await Deno.remove(noauthDir, { recursive: true }).catch(() => {});
    // Revocation kills a previously good token.
    revokeSessionToken(ctx.sid);
    const gone = await fetch(`${ctx.url}/info/refs?service=git-upload-pack`, {
      headers: { authorization: `Bearer ${ctx.token}` },
    });
    assertEquals(gone.status, 401);
    await gone.body?.cancel();
  } finally {
    await ctx.cleanup();
  }
});

Deno.test("gateway: receive serializes with host ref writers via withLock", async () => {
  const ctx = await setup("gw-s5");
  try {
    const clone = await bootstrapClone(ctx.url, ctx.sid, ctx.token);
    await Deno.writeTextFile(`${clone}/race.txt`, "race\n");
    await sh(clone, "git", "add", "-A");
    await sh(clone, "git", "commit", "-q", "-m", "race");
    // Hold the store lock as a host writer would (accept/adopt run inside it).
    let release!: () => void;
    const held = new Promise<void>((r) => (release = r));
    const lock = shadow.withLock(ctx.store, () => held);
    // A push during the hold must not land...
    const push = new Deno.Command("git", {
      args: ["push", "-q", "origin", `HEAD:refs/bough/sessions/${ctx.sid}`],
      cwd: clone,
      stdout: "piped",
      stderr: "piped",
    }).spawn();
    await new Promise((r) => setTimeout(r, 400));
    const tipDuring = (await sh(ctx.store, "git", "rev-parse", `refs/bough/sessions/${ctx.sid}`))
      .trim();
    const cloneHead = (await sh(clone, "git", "rev-parse", "HEAD")).trim();
    assert(tipDuring !== cloneHead, "receive waited for the held store lock");
    // ...and must land cleanly once the writer releases.
    release();
    await lock;
    const out = await push.output();
    assertEquals(out.code, 0, new TextDecoder().decode(out.stderr));
    const tipAfter = (await sh(ctx.store, "git", "rev-parse", `refs/bough/sessions/${ctx.sid}`))
      .trim();
    assertEquals(tipAfter, cloneHead);
    // The host-side seal still CASes cleanly after the push (no lost update):
    await shadow.accept(ctx.store, ctx.sid, "seal");
    const base = (await sh(ctx.store, "git", "rev-parse", `refs/bough/base/${ctx.sid}`)).trim();
    assertEquals(base, cloneHead);
    await Deno.remove(clone, { recursive: true }).catch(() => {});
  } finally {
    await ctx.cleanup();
  }
});

Deno.test("gateway: mint is per-session; a token can't cross sessions", async () => {
  const ctx = await setup("gw-s6");
  try {
    const otherToken = mintSessionToken("gw-other");
    const r = await fetch(`${ctx.url}/info/refs?service=git-upload-pack`, {
      headers: { authorization: `Bearer ${otherToken}` },
    });
    assertEquals(r.status, 401);
    await r.body?.cancel();
    revokeSessionToken("gw-other");
    // The right token still works.
    const ok = await fetch(`${ctx.url}/info/refs?service=git-upload-pack`, {
      headers: { authorization: `Bearer ${ctx.token}` },
    });
    assertEquals(ok.status, 200);
    assertEquals(
      ok.headers.get("content-type"),
      "application/x-git-upload-pack-advertisement",
    );
    const text = await ok.text();
    assertStringIncludes(text, "# service=git-upload-pack");
    assertStringIncludes(text, `refs/bough/sessions/${ctx.sid}`);
  } finally {
    await ctx.cleanup();
  }
});

Deno.test("gateway: unknown session id under a valid token shape is 404", async () => {
  const ctx = await setup("gw-s7");
  try {
    const tok = mintSessionToken("gw-ghost");
    const url = gitGatewayUrl("gw-ghost");
    const r = await fetch(`${url}/info/refs?service=git-upload-pack`, {
      headers: { authorization: `Bearer ${tok}` },
    });
    assertEquals(r.status, 404); // resolver knows no origin for gw-ghost
    await r.body?.cancel();
    revokeSessionToken("gw-ghost");
  } finally {
    await ctx.cleanup();
  }
});

Deno.test("gateway: worktree-mode createSessionWorkspace is untouched by the opt", async () => {
  // Regression guard for the { worktree } split: default behavior still builds
  // a checked-out worktree with hydration-visible files.
  const shadowBase = await Deno.makeTempDir({ prefix: "bough-gw-wt-store-" });
  const wsBase = await Deno.makeTempDir({ prefix: "bough-gw-wt-ws-" });
  Deno.env.set("BOUGH_SHADOW_BASE", shadowBase);
  Deno.env.set("BOUGH_SUBAGENT_BASE", wsBase);
  const repo = await makeRepo();
  try {
    const dir = await shadow.createSessionWorkspace(repo, "gw-wt");
    assert(await pathExists(`${dir}/.git`), "default mode still creates a worktree");
    assertEquals(await Deno.readTextFile(`${dir}/committed.txt`), "committed\n");
  } finally {
    Deno.env.delete("BOUGH_SHADOW_BASE");
    Deno.env.delete("BOUGH_SUBAGENT_BASE");
    for (const d of [shadowBase, wsBase, repo]) {
      await Deno.remove(d, { recursive: true }).catch(() => {});
    }
  }
});
