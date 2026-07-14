import { assert, assertStringIncludes } from "jsr:@std/assert@1";
import { bashBg, bashKill, bashOutput } from "./bash_bg.ts";

/** Poll `fn` until `pred` holds, collecting every returned chunk. */
async function pollUntil(
  fn: () => string,
  pred: (s: string) => boolean,
  ms = 10_000,
): Promise<string[]> {
  const seen: string[] = [];
  const deadline = Date.now() + ms;
  for (;;) {
    const s = fn();
    seen.push(s);
    if (pred(s)) return seen;
    if (Date.now() > deadline) throw new Error(`timed out polling; last: "${s}"`);
    await new Promise((r) => setTimeout(r, 50));
  }
}

function ctxIn(workspace: string) {
  return { workspace, sessionId: crypto.randomUUID() };
}

Deno.test("bashBg: returns a handle immediately, output reads are incremental", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-" });
  const ctx = ctxIn(workspace);
  try {
    const started = Date.now();
    const { id, pid } = JSON.parse(await bashBg("echo one; sleep 0.3; echo two", ctx));
    assert(Date.now() - started < 2_000, "bashBg should not block on the command");
    assert(typeof pid === "number");

    const polls = await pollUntil(
      () => bashOutput(id, ctx),
      (s) => s.includes("[exited with code 0]"),
    );
    const all = polls.join("\n");
    assertStringIncludes(all, "one");
    assertStringIncludes(all, "two");
    // Incremental contract: consumed output is never repeated.
    assert(all.indexOf("one") === all.lastIndexOf("one"), "output was repeated across reads");

    // A poll after exit reports the status without inventing output.
    assertStringIncludes(bashOutput(id, ctx), "(no new output)");
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

Deno.test("bashKill terminates a running shell and reports its end", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-kill-" });
  const ctx = ctxIn(workspace);
  try {
    const { id } = JSON.parse(await bashBg("sleep 30", ctx));
    assertStringIncludes(bashOutput(id, ctx), "[running]");
    assertStringIncludes(bashKill(id, ctx), "SIGTERM");
    await pollUntil(() => bashOutput(id, ctx), (s) => s.includes("[exited"));
    // A second kill is a no-op report, not an error.
    assertStringIncludes(bashKill(id, ctx), "already exited");
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

Deno.test("background shells are scoped to their session; unknown ids reject", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-scope-" });
  const a = ctxIn(workspace);
  const b = ctxIn(workspace);
  try {
    const { id } = JSON.parse(await bashBg("echo scoped", a));
    let msg = "";
    try {
      bashOutput(id, b);
    } catch (e) {
      msg = (e as Error).message;
    }
    assertStringIncludes(msg, "no background shell");
    // Drain in the owning session so the test leaves nothing running.
    await pollUntil(() => bashOutput(id, a), (s) => s.includes("[exited"));
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});
