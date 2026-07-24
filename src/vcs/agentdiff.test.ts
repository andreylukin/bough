import { assert, assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import * as agentdiff from "./agentdiff.ts";

Deno.test("parseAgentfsDiff maps A/M/D, drops dirs and AppleDouble sidecars", () => {
  const out = [
    "A f /._added.txt",
    "A f /added.txt",
    "M f /existing.txt",
    "D f /gone.txt",
    "A d /sub",
    "A f /sub/._nested.txt",
    "A f /sub/nested.txt",
  ].join("\n");
  const entries = agentdiff.parseAgentfsDiff(out);
  assertEquals(entries, [
    { status: "added", path: "added.txt" },
    { status: "modified", path: "existing.txt" },
    { status: "deleted", path: "gone.txt" },
    { status: "added", path: "sub/nested.txt" },
  ]);
});

Deno.test("deltaDbPath is HOME-anchored under .agentfs/run", () => {
  const prev = Deno.env.get("HOME");
  try {
    Deno.env.set("HOME", "/tmp/fake-home");
    assertEquals(agentdiff.deltaDbPath("sid-123"), "/tmp/fake-home/.agentfs/run/sid-123/delta.db");
  } finally {
    if (prev === undefined) Deno.env.delete("HOME");
    else Deno.env.set("HOME", prev);
  }
});

// --- integration: drive a real `agentfs run` overlay -------------------------

async function agentfsUsable(): Promise<boolean> {
  try {
    const r = await new Deno.Command("agentfs", {
      args: ["--version"],
      stdout: "null",
      stderr: "null",
    })
      .output();
    return r.code === 0;
  } catch {
    return false;
  }
}
const AGENTFS = await agentfsUsable();

/** Run a shell command inside the session overlay of `baseDir` (HOME=home so the
 *  delta lands where deltaDbPath expects it). */
async function overlayRun(home: string, baseDir: string, sid: string, cmd: string): Promise<void> {
  const r = await new Deno.Command("agentfs", {
    args: ["run", "--session", sid, "--", "/bin/sh", "-c", cmd],
    cwd: baseDir,
    env: { ...Deno.env.toObject(), HOME: home },
    stdout: "null",
    stderr: "null",
  }).output();
  if (r.code !== 0) throw new Error(`overlay run failed (${r.code})`);
}

/** Point HOME at a temp dir for the body, so agentfs run dirs are isolated + reaped. */
async function withHome(fn: (home: string) => Promise<void>): Promise<void> {
  const home = await Deno.makeTempDir({ prefix: "agentdiff-home-" });
  const prev = Deno.env.get("HOME");
  Deno.env.set("HOME", home);
  try {
    await fn(home);
  } finally {
    if (prev === undefined) Deno.env.delete("HOME");
    else Deno.env.set("HOME", prev);
    await Deno.remove(home, { recursive: true }).catch(() => {});
  }
}

Deno.test({
  name: "diff reports add/modify/delete from a live overlay",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      try {
        await Deno.writeTextFile(join(base, "keep.txt"), "keep\n");
        await Deno.writeTextFile(join(base, "mod.txt"), "a\nb\nc\n");
        await Deno.writeTextFile(join(base, "del.txt"), "gone\n");
        const sid = `t-diff-${crypto.randomUUID()}`;
        await overlayRun(
          home,
          base,
          sid,
          `printf 'a\nB\nc\n' > mod.txt; printf 'fresh\n' > add.txt; rm del.txt`,
        );
        const d = await agentdiff.diff(base, sid);
        assertEquals(d.source, "shadow");
        const byPath = new Map(d.files.map((f) => [f.path, f.status]));
        assertEquals(byPath.get("add.txt"), "added");
        assertEquals(byPath.get("mod.txt"), "modified");
        assertEquals(byPath.get("del.txt"), "deleted");
        assert(!byPath.has("keep.txt"), "untouched file must not appear");
        const mod = d.files.find((f) => f.path === "mod.txt")!;
        assert(mod.hunks.length > 0, "modified file has hunks");
        assert(mod.hunks[0].lines.some((l) => l === "-b"), "shows removed line");
        assert(mod.hunks[0].lines.some((l) => l === "+B"), "shows added line");
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "materialize delivers tip content into the origin checkout",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      const origin = await Deno.makeTempDir({ prefix: "agentdiff-origin-" });
      try {
        await Deno.writeTextFile(join(base, "mod.txt"), "a\nb\nc\n");
        await Deno.writeTextFile(join(origin, "mod.txt"), "a\nb\nc\n"); // origin == base
        const sid = `t-mat-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'a\nB\nc\n' > mod.txt; printf 'new\n' > add.txt`);
        const delivered = await agentdiff.materialize(base, sid, origin, []);
        assertEquals(delivered.sort(), ["add.txt", "mod.txt"]);
        assertEquals(await Deno.readTextFile(join(origin, "mod.txt")), "a\nB\nc\n");
        assertEquals(await Deno.readTextFile(join(origin, "add.txt")), "new\n");
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
        await Deno.remove(origin, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "accept folds the delta into base and clears the diff",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      try {
        await Deno.writeTextFile(join(base, "mod.txt"), "a\n");
        const sid = `t-acc-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'a\nb\n' > mod.txt; printf 'x\n' > add.txt; `);
        assert((await agentdiff.diff(base, sid)).files.length > 0);
        await agentdiff.accept(base, sid);
        // Base now carries the work…
        assertEquals(await Deno.readTextFile(join(base, "mod.txt")), "a\nb\n");
        assertEquals(await Deno.readTextFile(join(base, "add.txt")), "x\n");
        // …and the delta is gone, so the rail is clean.
        assertEquals(await agentdiff.hasDelta(sid), false);
        assertEquals((await agentdiff.diff(base, sid)).files.length, 0);
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
      }
    });
  },
});

Deno.test({
  name: "undoAll discards the whole delta back to base",
  ignore: !AGENTFS,
  fn: async () => {
    await withHome(async (home) => {
      const base = await Deno.makeTempDir({ prefix: "agentdiff-base-" });
      try {
        await Deno.writeTextFile(join(base, "mod.txt"), "a\n");
        const sid = `t-undo-${crypto.randomUUID()}`;
        await overlayRun(home, base, sid, `printf 'CHANGED\n' > mod.txt`);
        const reverted = await agentdiff.undoAll(base, sid);
        assertEquals(reverted, ["mod.txt"]);
        assertEquals(await agentdiff.hasDelta(sid), false);
        assertEquals(await Deno.readTextFile(join(base, "mod.txt")), "a\n"); // base pristine
      } finally {
        await Deno.remove(base, { recursive: true }).catch(() => {});
      }
    });
  },
});
