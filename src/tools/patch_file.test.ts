import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { patchFile, viewFile } from "./patch_file.ts";
import type { ToolRunCtx } from "./types.ts";

const SRC = `function add(a, b) {
  return a + b;
}

function sub(a, b) {
  return a - b;
}
`;

async function fixture(): Promise<{ dir: string; ctx: (id: string) => ToolRunCtx }> {
  const dir = await Deno.makeTempDir({ prefix: "patch-" });
  await Deno.writeTextFile(`${dir}/m.ts`, SRC);
  return { dir, ctx: (sessionId: string) => ({ workspace: dir, sessionId }) };
}

/** Pull the four-hex tag out of a view() header. */
function tagFrom(view: string): string {
  return /#([0-9A-F]{4})\]/.exec(view)![1];
}

Deno.test("view returns the tag header and numbered lines", async () => {
  const { dir, ctx } = await fixture();
  try {
    const out = await viewFile.run({ path: "m.ts" }, ctx("s1"));
    assertStringIncludes(out, "[m.ts#");
    assertStringIncludes(out, "1:function add(a, b) {");
    assertStringIncludes(out, "7:}");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("patch applies ops and echoes the new tag", async () => {
  const { dir, ctx } = await fixture();
  try {
    const c = ctx("s1");
    const tag = tagFrom(await viewFile.run({ path: "m.ts" }, c));
    const res = await patchFile.run({
      input: `[m.ts#${tag}]\nSWAP 2.=2:\n+  return a + b + 1;`,
    }, c);
    assertStringIncludes(res, "patched");
    assertStringIncludes(await Deno.readTextFile(`${dir}/m.ts`), "return a + b + 1;");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("the echoed tag chains: a second patch needs no re-read", async () => {
  const { dir, ctx } = await fixture();
  try {
    const c = ctx("s1");
    const tag = tagFrom(await viewFile.run({ path: "m.ts" }, c));
    const first = await patchFile.run({
      input: `[m.ts#${tag}]\nSWAP 2.=2:\n+  return a + b + 1;`,
    }, c);
    // The response carries the post-write tag; a follow-up patch uses it directly.
    const next = tagFrom(first);
    await patchFile.run({ input: `[m.ts#${next}]\nINS.TAIL:\n+// done` }, c);
    assertStringIncludes(await Deno.readTextFile(`${dir}/m.ts`), "// done");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("PARALLEL: a concurrent edit elsewhere rebases — both changes survive", async () => {
  const { dir, ctx } = await fixture();
  try {
    const a = ctx("agent-a");
    // A views the file and plans an edit to add().
    const tagA = tagFrom(await viewFile.run({ path: "m.ts" }, a));
    // B lands a change to sub() first, shifting nothing A named but changing the tag.
    const b = ctx("agent-b");
    const tagB = tagFrom(await viewFile.run({ path: "m.ts" }, b));
    await patchFile.run({
      input: `[m.ts#${tagB}]\nINS.PRE 5:\n+// B was here`,
    }, b);
    // A's patch is anchored to a stale tag but its lines are untouched.
    await patchFile.run({ input: `[m.ts#${tagA}]\nSWAP 2.=2:\n+  return a + b + 1;` }, a);

    const final = await Deno.readTextFile(`${dir}/m.ts`);
    assertStringIncludes(final, "return a + b + 1;"); // A landed
    assertStringIncludes(final, "// B was here"); // B survived
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("PARALLEL: a genuine conflict is refused, with the current text quoted", async () => {
  const { dir, ctx } = await fixture();
  try {
    const a = ctx("agent-a");
    const tagA = tagFrom(await viewFile.run({ path: "m.ts" }, a));
    const b = ctx("agent-b");
    const tagB = tagFrom(await viewFile.run({ path: "m.ts" }, b));
    // B rewrites the very line A is about to replace.
    await patchFile.run({ input: `[m.ts#${tagB}]\nSWAP 2.=2:\n+  return a + b + 100; // B` }, b);

    const err = await assertRejects(
      () => patchFile.run({ input: `[m.ts#${tagA}]\nSWAP 2.=2:\n+  return a + b + 1; // A` }, a),
      Error,
    );
    assertStringIncludes(err.message, "edited by someone else");
    assertStringIncludes(err.message, "// B"); // the live text, for a one-round fix
    // Nothing was written: B's line is intact and A's is absent.
    const final = await Deno.readTextFile(`${dir}/m.ts`);
    assertStringIncludes(final, "// B");
    assertEquals(final.includes("// A"), false);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a tag with no snapshot to rebase from is refused, not guessed", async () => {
  const { dir, ctx } = await fixture();
  try {
    // Never viewed, and the tag is invented — there is no base text to rebase.
    const err = await assertRejects(
      () => patchFile.run({ input: `[m.ts#0000]\nSWAP 2.=2:\n+  nope;` }, ctx("s1")),
      Error,
    );
    assertStringIncludes(err.message, "no longer held");
    assertEquals(await Deno.readTextFile(`${dir}/m.ts`), SRC);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a multi-file patch is all-or-nothing", async () => {
  const { dir, ctx } = await fixture();
  try {
    const c = ctx("s1");
    await Deno.writeTextFile(`${dir}/other.ts`, "const x = 1;\n");
    const tag = tagFrom(await viewFile.run({ path: "m.ts" }, c));
    // The second section is anchored to a tag that cannot resolve, so neither writes.
    await assertRejects(
      () =>
        patchFile.run({
          input:
            `[m.ts#${tag}]\nSWAP 2.=2:\n+  return 0;\n\n[other.ts#0000]\nSWAP 1.=1:\n+const x = 2;`,
        }, c),
      Error,
    );
    assertEquals(await Deno.readTextFile(`${dir}/m.ts`), SRC);
    assertEquals(await Deno.readTextFile(`${dir}/other.ts`), "const x = 1;\n");
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("no escaping: template-literal source round-trips verbatim", async () => {
  // The failure this format exists to prevent — a session lost six rounds trying
  // to reproduce this exact line inside its own template literal.
  const { dir, ctx } = await fixture();
  try {
    const c = ctx("s1");
    const body = "    const abs = `${root}/${rel}`;";
    const tag = tagFrom(await viewFile.run({ path: "m.ts" }, c));
    await patchFile.run({ input: `[m.ts#${tag}]\nSWAP 2.=2:\n+${body}` }, c);
    assertStringIncludes(await Deno.readTextFile(`${dir}/m.ts`), body);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
