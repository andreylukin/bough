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

import { bashWait } from "./bash_bg.ts";
import { bash } from "./bash.ts";
import { assertEquals } from "jsr:@std/assert@1";

Deno.test("bashBg: exit posts a completion note via ctx.notify (no polling needed)", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-notify-" });
  const notes: string[] = [];
  const ctx = { workspace, sessionId: crypto.randomUUID(), notify: (t: string) => notes.push(t) };
  try {
    const { id } = JSON.parse(await bashBg("echo done", ctx));
    await pollUntil(() => bashOutput(id, ctx), (s) => s.includes("[exited"));
    // The note lands shortly after exit (fired from the status handler).
    await pollUntil(() => (notes.length ? "yes" : ""), (s) => s === "yes", 3_000);
    assertStringIncludes(notes[0], "[background]");
    assertStringIncludes(notes[0], id);
    assertStringIncludes(notes[0], "finished");
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

Deno.test("bashWait: blocks until exit, returns the result, and suppresses the note", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-wait-" });
  const notes: string[] = [];
  const ctx = { workspace, sessionId: crypto.randomUUID(), notify: (t: string) => notes.push(t) };
  try {
    const { id } = JSON.parse(await bashBg("sleep 0.3; echo late", ctx));
    const out = await bashWait(id, ctx); // resolves only after exit
    assertStringIncludes(out, "late");
    assertStringIncludes(out, "[exited with code 0]");
    // Claimed in-band → no completion note fired.
    await new Promise((r) => setTimeout(r, 200));
    assertEquals(notes.length, 0);
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

Deno.test("bash: a command still running at the threshold auto-backgrounds, readable mid-run", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-auto-" });
  const notes: string[] = [];
  const ctx = { workspace, sessionId: crypto.randomUUID(), notify: (t: string) => notes.push(t) };
  const prev = Deno.env.get("BOUGH_BASH_BG_AFTER_MS");
  Deno.env.set("BOUGH_BASH_BG_AFTER_MS", "300"); // background fast for the test
  try {
    // Emits a line early, then keeps running past the threshold.
    const out = await bash.run({ command: "echo starting; sleep 1.5; echo finished" }, ctx);
    assertStringIncludes(out, "moved to background as");
    assertStringIncludes(out, "starting"); // accrued output rides along
    const m = out.match(/background as (bg_\d+)/);
    assert(m, "expected a bg id in the note");
    const id = m![1];

    // Read progress WHILE it is still running (the requirement).
    assertStringIncludes(bashOutput(id, ctx), "[running]");

    // Block for the rest; the tail arrives, exit is reported.
    const final = await bashWait(id, ctx);
    assertStringIncludes(final, "finished");
    assertStringIncludes(final, "[exited with code 0]");
  } finally {
    if (prev === undefined) Deno.env.delete("BOUGH_BASH_BG_AFTER_MS");
    else Deno.env.set("BOUGH_BASH_BG_AFTER_MS", prev);
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});

Deno.test("bash: a fast command still returns inline (no backgrounding)", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bg-fast-" });
  const ctx = { workspace, sessionId: crypto.randomUUID() };
  try {
    const out = await bash.run({ command: "echo quick" }, ctx);
    assertEquals(out, "quick");
    const err = await bash.run({ command: "echo oops >&2; exit 3" }, ctx);
    assertStringIncludes(err, "oops");
    assertStringIncludes(err, "[exit code 3]");
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});
