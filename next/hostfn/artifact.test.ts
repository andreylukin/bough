/**
 * Tests for the `artifact()` host function.
 *
 * What this file is for, over and above `server/artifacts.test.ts`: the bridge
 * contract and the REFUSAL TEXT. The wire is string-only, so a program receives JSON
 * and a failure receives a message — and the message is what the next round reasons
 * over. A refusal that says only "failed" is a defect (spec §6), so the test asserts
 * the sentence names the move: publish under a plain relative name.
 *
 * The other property asserted here is the scoping: a program never gets to name a
 * session, so the store it writes to is `ctx.sessionId`'s and nothing else. That is
 * the confinement that matters at this layer — a subagent publishing into its own
 * directory rather than its spawner's is not an accident of the caller, it is
 * structural.
 *
 * Hermetic: `root` is a temp directory, so nothing touches the real `~/.bough`.
 */
import assert from "node:assert/strict";
import { join } from "node:path";
import { ArtifactError } from "../errors.ts";
import type { TurnCtx } from "../types.ts";
import { createArtifactHostFn, listArtifacts } from "./artifact.ts";

function tmp(): string {
  return Deno.makeTempDirSync({ prefix: "bough-hostfn-artifact-" });
}

/** A fabricated turn context — no server, no database reads on this path. */
function turnCtx(sessionId: string): TurnCtx {
  return {
    db: null as never,
    bus: null as never,
    sessionId,
    turnId: "t1",
    messageId: "m1",
    workspace: Deno.cwd(),
    model: "test-model",
    signal: new AbortController().signal,
    depth: 0,
  };
}

Deno.test("artifact() writes into the session's store and returns url + href as JSON", async () => {
  const root = tmp();
  try {
    const { artifact } = createArtifactHostFn(turnCtx("sX"), {
      root,
      baseUrl: "http://127.0.0.1:4321",
    });
    const result = JSON.parse(await artifact!("index.html", "<h1>report</h1>")) as {
      name: string;
      url: string;
      href: string;
      bytes: number;
    };
    assert.deepEqual(result, {
      name: "index.html",
      url: "/artifacts/sX/index.html",
      href: "http://127.0.0.1:4321/artifacts/sX/index.html",
      bytes: "<h1>report</h1>".length,
    });
    assert.equal(Deno.readTextFileSync(join(root, "sX", "index.html")), "<h1>report</h1>");
  } finally {
    Deno.removeSync(root, { recursive: true });
  }
});

Deno.test("artifact() is scoped to its own session — it cannot name another's", async () => {
  const root = tmp();
  try {
    const spawner = createArtifactHostFn(turnCtx("spawner"), { root });
    const child = createArtifactHostFn(turnCtx("child"), { root });
    await spawner.artifact!("a.html", "spawner");
    await child.artifact!("a.html", "child");

    assert.equal(Deno.readTextFileSync(join(root, "spawner", "a.html")), "spawner");
    assert.equal(Deno.readTextFileSync(join(root, "child", "a.html")), "child");
    assert.deepEqual(listArtifacts("child", { root }).map((a) => a.name), ["a.html"]);

    // Reaching sideways is a path escape, not a write into the sibling's store.
    await assert.rejects(() => child.artifact!("../spawner/a.html", "pwned"));
    assert.equal(Deno.readTextFileSync(join(root, "spawner", "a.html")), "spawner");
  } finally {
    Deno.removeSync(root, { recursive: true });
  }
});

Deno.test("an escaping name is refused with text naming the move, and writes nothing", async () => {
  const root = tmp();
  try {
    const { artifact } = createArtifactHostFn(turnCtx("sY"), { root });
    for (const bad of ["../escape.html", "sub/../../escape.html", ""]) {
      const err = await artifact!(bad, "pwned").then(() => null, (e: unknown) => e);
      assert.equal(err instanceof ArtifactError, true, `expected a refusal for ${bad}`);
      const message = (err as Error).message;
      assert.equal(message.includes("escapes this session's artifact directory"), true);
      assert.equal(message.includes("plain relative name"), true);
      assert.equal(message.includes("index.html"), true);
      assert.equal(message.includes("nothing was written"), true);
    }
    assert.deepEqual([...Deno.readDirSync(root)].map((e) => e.name), []);
  } finally {
    Deno.removeSync(root, { recursive: true });
  }
});

Deno.test("republishing overwrites in place so an open link keeps working", async () => {
  const root = tmp();
  try {
    const { artifact } = createArtifactHostFn(turnCtx("sZ"), { root });
    const first = JSON.parse(await artifact!("page.html", "v1")) as { url: string };
    const second = JSON.parse(await artifact!("page.html", "v2-longer")) as {
      url: string;
      bytes: number;
    };
    assert.equal(second.url, first.url);
    assert.equal(second.bytes, "v2-longer".length);
    assert.deepEqual(listArtifacts("sZ", { root }).map((a) => a.name), ["page.html"]);
  } finally {
    Deno.removeSync(root, { recursive: true });
  }
});

Deno.test("nested asset paths publish and list with forward slashes", async () => {
  const root = tmp();
  try {
    const { artifact } = createArtifactHostFn(turnCtx("sN"), { root });
    await artifact!("index.html", "<html></html>");
    const asset = JSON.parse(await artifact!("assets/app.js", "console.log(1)")) as {
      name: string;
      url: string;
    };
    assert.equal(asset.name, "assets/app.js");
    assert.equal(asset.url, "/artifacts/sN/assets/app.js");
    assert.deepEqual(
      listArtifacts("sN", { root }).map((a) => a.name).sort(),
      ["assets/app.js", "index.html"],
    );
  } finally {
    Deno.removeSync(root, { recursive: true });
  }
});
