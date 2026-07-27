/**
 * Tests for the change set — the surface a user reviews before delivering.
 *
 * These run against a REAL git repository in a temp dir rather than a fake,
 * because every bug this file has caught was a disagreement between what bough
 * reported and what `git status` reports, and a fake git agrees with itself.
 */
import assert from "node:assert/strict";
import { join } from "node:path";
import { changeSet } from "./repodiff.ts";

/** A throwaway repo with one commit. Returns its path; the caller removes it. */
async function repo(): Promise<{ dir: string; head: string }> {
  const dir = await Deno.makeTempDir({ prefix: "bough-repodiff-" });
  const run = async (...args: string[]) => {
    const c = new Deno.Command("git", {
      args,
      cwd: dir,
      stdout: "piped",
      stderr: "piped",
      env: { GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
    const out = await c.output();
    assert.ok(out.success, `git ${args.join(" ")} failed`);
    return new TextDecoder().decode(out.stdout).trim();
  };
  await run("init", "-q", "-b", "main");
  await run("config", "user.email", "t@t");
  await run("config", "user.name", "t");
  await Deno.writeTextFile(join(dir, "tracked.txt"), "one\n");
  await run("add", "-A");
  await run("commit", "-qm", "init");
  return { dir, head: await run("rev-parse", "HEAD") };
}

Deno.test("a RUNAWAY untracked directory collapses to one entry, the way git status shows it", async () => {
  const { dir, head } = await repo();
  try {
    // The shape that broke the rail in bough's own checkout: one untracked
    // directory holding far more files than a reviewer will ever scroll.
    await Deno.mkdir(join(dir, "bench", "state"), { recursive: true });
    for (let i = 0; i < 60; i++) {
      await Deno.writeTextFile(join(dir, "bench", "state", `r${i}.json`), `{"i":${i}}\n`);
    }
    await Deno.writeTextFile(join(dir, "loose.txt"), "new\n");
    // A small new directory is the agent's actual work and stays itemized.
    await Deno.mkdir(join(dir, "feature"), { recursive: true });
    await Deno.writeTextFile(join(dir, "feature", "a.ts"), "export const a = 1;\n");
    await Deno.writeTextFile(join(dir, "feature", "b.ts"), "export const b = 2;\n");

    const set = await changeSet(dir, head);
    assert.ok(set.available);
    const paths = set.files.map((f) => f.path).sort();
    // One entry for the whole directory — not 60 — plus the loose file.
    assert.deepEqual(paths, ["bench/", "feature/a.ts", "feature/b.ts", "loose.txt"]);
    // The collapsed directory carries no body: there is no single file to show.
    assert.deepEqual(set.files.find((f) => f.path === "bench/")?.hunks, []);
    // The loose file still gets its contents, because that IS reviewable.
    assert.deepEqual(
      set.files.find((f) => f.path === "loose.txt")?.hunks?.[0]?.lines,
      ["+new"],
    );
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a tracked edit and an untracked file are both reported, and not twice", async () => {
  const { dir, head } = await repo();
  try {
    await Deno.writeTextFile(join(dir, "tracked.txt"), "one\ntwo\n");
    await Deno.writeTextFile(join(dir, "added.txt"), "fresh\n");
    const set = await changeSet(dir, head);
    assert.ok(set.available);
    assert.deepEqual(set.files.map((f) => f.path).sort(), ["added.txt", "tracked.txt"]);
    assert.equal(set.files.find((f) => f.path === "tracked.txt")?.status, "modified");
    assert.equal(set.files.find((f) => f.path === "added.txt")?.status, "added");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a huge untracked file is listed but not inlined", async () => {
  const { dir, head } = await repo();
  try {
    // Over MAX_ADDED_BYTES. It must still appear — you have to be able to see that
    // the file is new — but nobody reviews a megabyte by scrolling it.
    await Deno.writeTextFile(join(dir, "big.txt"), "x\n".repeat(400_000));
    const set = await changeSet(dir, head);
    assert.ok(set.available);
    const big = set.files.find((f) => f.path === "big.txt");
    assert.ok(big, "the file must still be listed");
    assert.deepEqual(big.hunks, []);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a directory that is not a repo answers, rather than failing", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-norepo-" });
  try {
    const set = await changeSet(dir, null);
    // Spec §13: unavailable is an ANSWER, and it says why.
    assert.equal(set.available, false);
    assert.match(set.reason ?? "", /not a git repository/);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
