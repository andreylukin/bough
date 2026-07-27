/**
 * Tests for the HTTP primitives, and — the reason this file exists — for the
 * ACYCLICITY of the server module graph.
 *
 * The behavioural tests below are ordinary: `json` sets its header, `errorResponse`
 * uses the one envelope every client reads, `parseBody` turns a schema failure into
 * the 400 the dispatcher renders, `route` compiles a pattern.
 *
 * The last two are the point. `server/app.ts` builds its route table at MODULE
 * SCOPE, naming a function from every handler module — so the table dereferences
 * those bindings while `app.ts` is still evaluating. As long as handler modules also
 * imported `json`/`parseBody`/`Handler` back from `app.ts`, that was a cycle with a
 * read inside it, and whether the process started depended on which module the graph
 * was entered through:
 *
 *   - through `app.ts` (what `main.ts` does): handlers evaluate first, fine;
 *   - through any handler module: `app.ts` evaluates mid-body and reads a `const`
 *     that has not been assigned → `ReferenceError: Cannot access 'listSessions'
 *     before initialization`, at IMPORT, before a line of user code.
 *
 * It was reproducible and it had been flagged in two phases. A comment saying "don't
 * import app.ts from a handler" is not a fix, because the failure appears in whoever
 * writes the next import — not in the file that made it fragile. So:
 *
 *   1. **the graph guard** walks every module under `next/` and asserts that nothing
 *      but the entry points imports `server/app.ts`, which is what keeps the graph a
 *      DAG whichever module is entered first;
 *   2. **the runtime probe** actually imports `server/sessions.ts` in a FRESH deno
 *      process, entering the graph through a handler module — the exact thing that
 *      used to throw. A static rule can be argued with; a subprocess that either
 *      prints OK or does not, cannot.
 *
 * The probe spawns `deno eval` with this package's config and no permission flags.
 * It is offline, touches no `~/.bough`, and needs no API key (plan §7).
 *
 * Assertions come from `node:assert/strict` — jsr.io is unreachable here and a test
 * that cannot run offline does not belong in `deno task test`.
 */
import assert from "node:assert/strict";
import { z } from "zod";
import { HttpError } from "../errors.ts";
import { errorResponse, json, parseBody, route } from "./http.ts";

const NEXT_ROOT = new URL("../", import.meta.url);
const CONFIG = new URL("../deno.json", import.meta.url);

// ---- the primitives ---------------------------------------------------------

Deno.test("json carries the JSON content type and the status it was given", async () => {
  const ok = json({ a: 1 });
  assert.equal(ok.status, 200);
  assert.equal(ok.headers.get("content-type"), "application/json; charset=utf-8");
  assert.deepEqual(await ok.json(), { a: 1 });

  assert.equal(json({ a: 1 }, 201).status, 201);
});

Deno.test("errorResponse is the one envelope every client reads", async () => {
  const res = errorResponse(404, "no session x");
  assert.equal(res.status, 404);
  assert.deepEqual(await res.json(), { error: "no session x" });
});

Deno.test("parseBody validates, and a bad body becomes a catchable 400", async () => {
  const Body = z.object({ name: z.string() });
  const req = (body: unknown) =>
    new Request("http://x/y", { method: "POST", body: JSON.stringify(body) });

  assert.deepEqual(await parseBody(req({ name: "a" }), Body), { name: "a" });

  const bad = await parseBody(req({ name: 1 }), Body).then(
    () => null,
    (e: unknown) => e,
  );
  assert.ok(bad instanceof HttpError, `expected HttpError, got ${bad}`);
  assert.equal((bad as HttpError).status, 400);
  assert.match((bad as HttpError).message, /invalid body/);
});

Deno.test("parseBody's fallback stands in for an absent or unparseable body", async () => {
  const AllOptional = z.object({ paths: z.array(z.string()).optional() });
  const empty = new Request("http://x/y", { method: "POST" });
  // `{}` is what a route with no required fields passes; without it the schema
  // would be handed `null` and reject a perfectly valid bodyless request.
  assert.deepEqual(await parseBody(empty, AllOptional, {}), {});
});

Deno.test("route compiles its pathname into a matcher and keeps the method verbatim", () => {
  const handler = () => json({});
  const r = route("POST", "/sessions/:id/messages", handler);
  assert.equal(r.method, "POST");
  assert.equal(r.handler, handler);
  assert.ok(r.pattern.exec({ pathname: "/sessions/abc/messages" }));
  assert.equal(r.pattern.exec({ pathname: "/sessions/abc" }), null);
});

// ---- the graph guard --------------------------------------------------------

/**
 * Modules allowed to import `server/app.ts`. Each is a graph ROOT — something a
 * process or a test enters through — so it cannot be the middle of a cycle.
 *
 * `server/http.ts` is not in this list and must never be: it is the module every
 * handler depends on, and an edge from it back to `app.ts` would recreate exactly
 * the cycle this file exists to prevent.
 */
const MAY_IMPORT_APP = new Set(["server/main.ts"]);

/** `import ... from "…/app.ts"`, in any of the spellings the tree uses. */
const APP_IMPORT = /from\s+"(?:\.{1,2}\/)+(?:server\/)?app\.ts"/;

/**
 * Comments stripped before the scan. Several modules — this one included —
 * legitimately QUOTE the forbidden import while explaining why it is forbidden, and
 * a guard that cannot tell a citation from an edge would make documenting the rule
 * a test failure.
 */
function code(src: string): string {
  return src.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
}

async function* modules(dir: URL, prefix = ""): AsyncGenerator<{ rel: string; path: URL }> {
  for await (const entry of Deno.readDir(dir)) {
    if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
    const child = new URL(entry.name + (entry.isDirectory ? "/" : ""), dir);
    const rel = prefix + entry.name;
    if (entry.isDirectory) yield* modules(child, rel + "/");
    else if (/\.tsx?$/.test(entry.name)) yield { rel, path: child };
  }
}

Deno.test("NOTHING but an entry point imports server/app.ts — the graph stays a DAG", async () => {
  const offenders: string[] = [];
  for await (const { rel, path } of modules(NEXT_ROOT)) {
    // A test file is a graph root by definition: deno enters through it, so it can
    // import `app.ts` (and `createHandler`/`routes` are exactly what a router test
    // needs) without ever being the middle of a cycle.
    if (rel.endsWith(".test.ts") || rel.endsWith(".test.tsx")) continue;
    if (rel === "server/app.ts" || MAY_IMPORT_APP.has(rel)) continue;
    if (APP_IMPORT.test(code(await Deno.readTextFile(path)))) offenders.push(rel);
  }
  assert.deepEqual(
    offenders,
    [],
    "these modules import server/app.ts, which re-forms the initialization cycle — " +
      "import the primitives from server/http.ts instead: " + offenders.join(", "),
  );
});

Deno.test("server/http.ts depends on nothing inside server/ — it is the bottom", async () => {
  const src = await Deno.readTextFile(new URL("http.ts", import.meta.url));
  const specs = [...code(src).matchAll(/^import\s[^"]*"([^"]+)"/gm)].map((m) => m[1]);
  const inServer = specs.filter((s) => s.startsWith("./") || s.includes("/server/"));
  assert.deepEqual(inServer, [], `http.ts must not import from server/: ${inServer.join(", ")}`);
});

// ---- the runtime probe ------------------------------------------------------

Deno.test({
  name: "entering the graph through a HANDLER module initializes — the cycle is gone",
  // Spawning a deno process is the whole point: the failure being pinned is a
  // module-initialization order fault, and it can only be observed by a process
  // that enters the graph somewhere other than where this test's own process did.
  fn: async () => {
    // Every handler module `app.ts` names in its table, entered directly. Before
    // the split, each of these threw on import.
    for (const mod of ["server/sessions.ts", "server/changes.ts", "server/search.ts"]) {
      const src = new URL(mod, NEXT_ROOT).href;
      const out = await new Deno.Command(Deno.execPath(), {
        args: [
          "eval",
          "--config",
          CONFIG.pathname,
          `await import(${JSON.stringify(src)}); console.log("PROBE_OK");`,
        ],
        stdout: "piped",
        stderr: "piped",
      }).output();
      const stdout = new TextDecoder().decode(out.stdout);
      const stderr = new TextDecoder().decode(out.stderr);
      assert.ok(
        out.success && stdout.includes("PROBE_OK"),
        `importing ${mod} first failed — the app.ts cycle is back:\n${stderr}`,
      );
    }
  },
});
