import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import * as jj from "./jj.ts";

async function sh(bin: string, args: string[], cwd: string): Promise<void> {
  const { code, stderr } = await new Deno.Command(bin, {
    args,
    cwd,
    stdout: "null",
    stderr: "piped",
  }).output();
  if (code !== 0) throw new Error(`${bin} ${args.join(" ")}: ${new TextDecoder().decode(stderr)}`);
}

/** A fresh git repo with one commit, in a temp dir. Returns its path. */
async function tempGitRepo(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "jjtest-" });
  await sh("git", ["init", "-q", "."], dir);
  await Deno.writeTextFile(`${dir}/README.md`, "base\n");
  await sh("git", ["add", "-A"], dir);
  await sh("git", [
    "-c",
    "user.email=t@t",
    "-c",
    "user.name=t",
    "commit",
    "-qm",
    "init",
  ], dir);
  return dir;
}

// These smokes shell out to jj + git, so they need `--allow-run`. Under the
// current `deno task test` flags they self-skip; run them for real with
// `deno test --allow-run` (see src/sandbox/INTEGRATION.md).
async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}

const jjAvailable = (await canRun("jj")) && (await canRun("git")) &&
  await (async () => {
    try {
      await jj.version();
      return true;
    } catch {
      return false;
    }
  })();

Deno.test({
  name: "jj: init → edit → diff → fork → undo round-trip",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      // version() reports jj.
      assertStringIncludes(await jj.version(), "jj");

      // ensureWorkspace creates the session change; base repo content is preserved.
      const name = await jj.ensureWorkspace(repo, "s1");
      assertEquals(name, "bough/s1");
      assertEquals((await Deno.readTextFile(`${repo}/README.md`)).trim(), "base");

      // Edit inside the workspace, then diff shows exactly that change.
      await Deno.writeTextFile(`${repo}/hello.txt`, "from-s1\n");
      const d1 = await jj.diff(repo, "s1");
      assertEquals(d1.source, "jj");
      const paths1 = d1.files.map((f) => f.path).sort();
      assertEquals(paths1, ["hello.txt"]);
      assertEquals(d1.files[0].status, "added");
      assertEquals(d1.files[0].hunks[0].lines, ["+from-s1"]);

      // Fork s1 → s2: the fork inherits s1's work, then diverges.
      const forked = await jj.forkSession(repo, "s1", "s2");
      assertEquals(forked, "bough/s2");
      await Deno.writeTextFile(`${repo}/only-s2.txt`, "s2-work\n");
      const d2 = await jj.diff(repo, "s2");
      assertEquals(d2.files.map((f) => f.path).sort(), ["only-s2.txt"]);

      // s1's diff is untouched by the fork's edits.
      const d1again = await jj.diff(repo, "s1");
      assertEquals(d1again.files.map((f) => f.path).sort(), ["hello.txt"]);

      // Idempotent resume: ensureWorkspace on an existing session switches to it.
      const resumed = await jj.ensureWorkspace(repo, "s1");
      assertEquals(resumed, "bough/s1");

      // Op log + undo: the last op (switching to s1) can be undone.
      const ops = await jj.operations(repo, 5);
      assert(ops.length > 0, "op log should be non-empty");
      await jj.undo(repo);

      // restore to a specific op id works (restore to the current tip is a no-op-safe call).
      await jj.restore(repo, ops[0].id);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  name: "jj: ensureWorkspace is idempotent and modified files diff correctly",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      await jj.ensureWorkspace(repo, "s1");
      await Deno.writeTextFile(`${repo}/README.md`, "base\nadded line\n");
      const d = await jj.diff(repo, "s1");
      assertEquals(d.files.length, 1);
      assertEquals(d.files[0].path, "README.md");
      assertEquals(d.files[0].status, "modified");
      assertEquals(d.files[0].hunks[0].lines, [" base", "+added line"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});

Deno.test({
  // Regression: a session opened on a repo with uncommitted + untracked changes
  // must NOT wipe them. `ensureWorkspace` used to `jj new <HEAD>`, resetting the
  // working copy to the committed tree and deleting in-progress work.
  name: "jj: ensureWorkspace preserves uncommitted and untracked files",
  ignore: !jjAvailable,
  fn: async () => {
    const repo = await tempGitRepo();
    try {
      // Dirty the repo the way a real workspace is dirty: an untracked new file
      // and an uncommitted edit to a tracked file.
      await Deno.writeTextFile(`${repo}/untracked.ts`, "export const x = 1;\n");
      await Deno.writeTextFile(`${repo}/README.md`, "base\nlocal edit\n");

      await jj.ensureWorkspace(repo, "s1");

      // Both must still be on disk.
      assertEquals(await Deno.readTextFile(`${repo}/untracked.ts`), "export const x = 1;\n");
      assertEquals(await Deno.readTextFile(`${repo}/README.md`), "base\nlocal edit\n");

      // And they belong to the pre-session baseline, so the session's own diff is
      // empty until the agent changes something (no pre-existing dirt leaks in).
      const d0 = await jj.diff(repo, "s1");
      assertEquals(d0.files.length, 0);

      // A genuine agent edit then shows up alone.
      await Deno.writeTextFile(`${repo}/untracked.ts`, "export const x = 2;\n");
      const d1 = await jj.diff(repo, "s1");
      assertEquals(d1.files.map((f) => f.path), ["untracked.ts"]);
    } finally {
      await Deno.remove(repo, { recursive: true });
    }
  },
});
