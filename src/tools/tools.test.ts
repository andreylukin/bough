import { assert, assertEquals, assertRejects } from "jsr:@std/assert@1";
import { bash } from "./bash.ts";
import { readFile } from "./read_file.ts";
import { writeFile } from "./write_file.ts";
import { editFile } from "./edit_file.ts";
import { jsonSchema } from "./types.ts";

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

Deno.test("file tools reject paths that escape the workspace", async () => {
  const { dir, ctx } = await tmp();
  try {
    await assertRejects(() => writeFile.run({ path: "../escape.txt", content: "x" }, ctx));
    await assertRejects(() => readFile.run({ path: "/etc/hosts" }, ctx));
    // With a sandbox handle, the snapshot dir and the scratchpad are also writable.
    const snap = `${dir}-snap`;
    const scratch = `${dir}-scratch`;
    await Deno.mkdir(snap, { recursive: true });
    await Deno.mkdir(scratch, { recursive: true });
    const sbx = { workspace: dir, sandbox: { sessionDir: snap, scratchDir: scratch } };
    await writeFile.run({ path: `${snap}/ok.txt`, content: "y" }, sbx);
    assertEquals(await Deno.readTextFile(`${snap}/ok.txt`), "y");
    await writeFile.run({ path: `${scratch}/tmp.txt`, content: "z" }, sbx);
    assertEquals(await Deno.readTextFile(`${scratch}/tmp.txt`), "z");
    await Deno.remove(snap, { recursive: true });
    await Deno.remove(scratch, { recursive: true });
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("file tools follow symlinks and reject ones that escape the workspace", async () => {
  const dir = await Deno.makeTempDir();
  const outside = await Deno.makeTempDir();
  try {
    await Deno.writeTextFile(`${outside}/secret.txt`, "top secret");
    await Deno.symlink(outside, `${dir}/link`); // a link inside the workspace → outside
    await assertRejects(() => readFile.run({ path: "link/secret.txt" }, { workspace: dir }));
  } finally {
    await Deno.remove(dir, { recursive: true });
    await Deno.remove(outside, { recursive: true });
  }
});

// The real Seatbelt integration: bash writes inside the workspace but is denied
// outside it. macOS-only (sandbox-exec); needs --allow-run.
Deno.test({
  name: "sandboxed bash writes inside the workspace but is denied outside",
  ignore: Deno.build.os !== "darwin",
  async fn() {
    const dir = await Deno.makeTempDir();
    const ctx = { workspace: dir, sandbox: { sessionDir: `${dir}/.snap`, scratchDir: `${dir}/.scratch` } };
    const escape = `${Deno.env.get("HOME")}/bough-seatbelt-escape-${crypto.randomUUID()}.txt`;
    try {
      await bash.run({ command: "echo hi > inside.txt" }, ctx);
      assertEquals(await Deno.readTextFile(`${dir}/inside.txt`), "hi\n");

      const out = await bash.run({ command: `echo pwn > '${escape}'` }, ctx);
      let leaked = false;
      try {
        await Deno.stat(escape);
        leaked = true;
      } catch {
        // denied as expected
      }
      assert(!leaked, "write outside the workspace should have been denied");
      assert(out.includes("exit code"), "denied write should report a non-zero exit");
    } finally {
      await Deno.remove(dir, { recursive: true });
      await Deno.remove(escape).catch(() => {});
    }
  },
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
