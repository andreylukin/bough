import { assert, assertEquals, assertRejects } from "jsr:@std/assert@1";
import { bash } from "./bash.ts";
import { readFile } from "./read_file.ts";
import { writeFile } from "./write_file.ts";
import { editFile } from "./edit_file.ts";
import { jsonSchema } from "./types.ts";
import { dirname } from "node:path";

async function canRun(cmd: string): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run", command: cmd })).state === "granted";
}

async function tmp(): Promise<{ dir: string; ctx: { workspace: string } }> {
  const dir = await Deno.makeTempDir();
  return { dir, ctx: { workspace: dir } };
}

Deno.test("write_file then read_file round-trips through the workspace", async () => {
  const { dir, ctx } = await tmp();
  try {
    await writeFile.run({ path: "sub/note.txt", content: "hello" }, ctx);
    const got = await readFile.run({ path: "sub/note.txt" }, ctx);
    assertEquals(got, "hello");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("edit_file replaces a unique match and rejects ambiguous or missing ones", async () => {
  const { dir, ctx } = await tmp();
  try {
    await writeFile.run({ path: "a.txt", content: "one two two" }, ctx);
    await assertRejects(() =>
      editFile.run({ path: "a.txt", old_string: "two", new_string: "x" }, ctx)
    );
    await assertRejects(() =>
      editFile.run({ path: "a.txt", old_string: "zzz", new_string: "x" }, ctx)
    );
    await editFile.run({ path: "a.txt", old_string: "one two two", new_string: "done" }, ctx);
    assertEquals(await readFile.run({ path: "a.txt" }, ctx), "done");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("jsonSchema emits a draft-7 object schema with required fields", () => {
  const schema = jsonSchema(writeFile) as {
    type: string;
    required: string[];
    properties: Record<string, unknown>;
  };
  assertEquals(schema.type, "object");
  assertEquals(schema.required.sort(), ["content", "path"]);
});

Deno.test("file tools resolve absolute paths anywhere — there is no confinement", async () => {
  const { dir, ctx } = await tmp();
  const outside = await Deno.makeTempDir();
  try {
    // The workspace is the ORIGIN for relative paths, not a boundary: the agent's
    // bash runs unconfined in the user's own account, so the file tools must reach
    // exactly as far or they'd disagree with it.
    await writeFile.run({ path: `${outside}/note.txt`, content: "out" }, ctx);
    assertEquals(await Deno.readTextFile(`${outside}/note.txt`), "out");
    assertEquals(await readFile.run({ path: `${outside}/note.txt` }, ctx), "out");
    // ...including via `..` out of the workspace and through a symlink that leaves it.
    await writeFile.run({ path: "../escape.txt", content: "up" }, ctx);
    assertEquals(await Deno.readTextFile(`${dirname(dir)}/escape.txt`), "up");
    await Deno.symlink(outside, `${dir}/link`);
    assertEquals(await readFile.run({ path: "link/note.txt" }, ctx), "out");
  } finally {
    await Deno.remove(`${dirname(dir)}/escape.txt`).catch(() => {});
    await Deno.remove(dir, { recursive: true });
    await Deno.remove(outside, { recursive: true });
  }
});

Deno.test("bash edits are visible to a later git command in the same workspace", async () => {
  // The single-view guarantee. Under the old copy-on-write overlay a shell write
  // landed in a per-session delta that git (running outside it) could not see, so
  // the agent's own `git status` lied to it. Both now touch the same bytes.
  if (!(await canRun("git"))) return;
  const dir = await Deno.makeTempDir({ prefix: "bough-onview-" });
  const ctx = { workspace: dir };
  try {
    await bash.run({ command: "git init -q ." }, ctx);
    await bash.run({ command: "printf 'hi\n' > tracked.txt" }, ctx);
    const status = await bash.run({ command: "git status --porcelain" }, ctx);
    assert(status.includes("tracked.txt"), `git could not see the shell's write:\n${status}`);
    // ...and the write_file tool shares that same view.
    await writeFile.run({ path: "viafile.txt", content: "x" }, ctx);
    const status2 = await bash.run({ command: "git status --porcelain" }, ctx);
    assert(status2.includes("viafile.txt"), status2);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("bash: the turn's interrupt signal kills the child process", async () => {
  const workspace = await Deno.makeTempDir({ prefix: "bough-bash-int-" });
  try {
    const controller = new AbortController();
    const started = Date.now();
    const run = bash.run({ command: "sleep 30" }, { workspace, signal: controller.signal });
    setTimeout(() => controller.abort(), 100);
    let msg = "";
    try {
      await run;
    } catch (e) {
      msg = (e as Error).message;
    }
    if (!msg.includes("turn interrupted")) throw new Error(`unexpected: "${msg}"`);
    if (Date.now() - started > 10_000) throw new Error("child was not killed promptly");
  } finally {
    await Deno.remove(workspace, { recursive: true }).catch(() => {});
  }
});
