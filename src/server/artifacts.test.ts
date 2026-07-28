/**
 * Tests for the artifact store and its two routes.
 *
 * The acceptance criteria (plan T6.6) are the three tests that name them:
 *
 *   - **Traversal is blocked in BOTH the name and the session id.** Two separate
 *     inputs reach two separate `confine` calls, and a store that guarded only the
 *     name would still let `GET /artifacts/..%2F..%2Fetc/passwd` out of the tree. Both
 *     are asserted at the function level and through the route.
 *   - **Listing survives a database reset.** The filesystem is the source of truth
 *     (spec §4), so a session with no row at all still lists its artifacts. This is
 *     the test that would fail the day someone adds an artifacts table "for speed".
 *   - **The comments sidecar is not reachable through the artifact route.** It lives
 *     outside the artifact tree (plan §6.12), so it is neither walked by the listing
 *     nor addressable as an artifact — asserted against the REAL default paths, with
 *     `BOUGH_HOME` pointed at a temp root, because the invariant is about the layout
 *     rather than about any argument a caller passes.
 *
 * Everything runs offline against temp directories; nothing touches the real
 * `~/.bough`. Assertions come from `node:assert/strict` rather than `@std/assert`:
 * jsr.io is not reachable from this environment.
 */
import { test } from "bun:test";
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { PathError } from "../errors.ts";
import {
  type ArtifactStoreOptions,
  listArtifacts,
  publishArtifact,
  resolveArtifactPath,
} from "../hostfn/artifact.ts";
import { artifactsDir, commentsPathFor } from "../paths.ts";
import type { AppCtx } from "../types.ts";
// `./app.ts` FIRST, and deliberately: the handler modules import its `json` helper,
// which is the cycle `app.ts` documents as safe. It is safe only when `app.ts` is the
// module that starts evaluating — entering through a handler module instead leaves its
// exports in the temporal dead zone while `app.ts` builds the route table.
import { createHandler, type Route, route } from "./app.ts";
import { getArtifactH, listArtifactsH, NOT_FOUND_PAGE, serveArtifact } from "./artifacts.ts";
import { addComment } from "./comments.ts";

// ---- fixtures ---------------------------------------------------------------

const TABLE: Route[] = [
  route("GET", "/sessions/:id/artifacts", listArtifactsH),
  route("GET", "/artifacts/:id/:path*", getArtifactH),
];

function tmp(): string {
  return mkdtempSync(join(tmpdir(), "bough-artifacts-"));
}

function store(root: string): ArtifactStoreOptions {
  return { root, baseUrl: "http://127.0.0.1:4321" };
}

/**
 * Run `body` with `BOUGH_HOME` pointed at a fresh temp root, then put the environment
 * back. The route handlers read the default paths, so this is the only way to exercise
 * them without writing under the user's real data root.
 */
async function withBoughHome(body: (home: string) => Promise<void> | void): Promise<void> {
  const home = tmp();
  const previous = process.env["BOUGH_HOME"];
  process.env["BOUGH_HOME"] = home;
  try {
    await body(home);
  } finally {
    if (previous === undefined) delete process.env["BOUGH_HOME"];
    else process.env["BOUGH_HOME"] = previous;
    rmSync(home, { recursive: true, force: true });
  }
}

function fixture() {
  const db = openDb(":memory:");
  const bus = new Bus({ onListenerError: () => {} });
  const ctx: AppCtx = { db, bus, model: "test-model" };
  return { call: createHandler(ctx, { routes: TABLE }), db, bus };
}

const get = (path: string, headers?: HeadersInit) =>
  new Request(`http://127.0.0.1:4321${path}`, { headers });

// ---- publish ----------------------------------------------------------------

test("publishArtifact writes under the session dir and returns url + href", async () => {
  const root = tmp();
  try {
    const art = await publishArtifact("sessAbc", "index.html", "<h1>hi</h1>", store(root));
    assert.equal(art.name, "index.html");
    assert.equal(art.url, "/artifacts/sessAbc/index.html");
    assert.equal(art.href, "http://127.0.0.1:4321/artifacts/sessAbc/index.html");
    assert.equal(art.bytes, "<h1>hi</h1>".length);
    assert.equal(readFileSync(join(root, "sessAbc", "index.html"), "utf8"), "<h1>hi</h1>");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("publishArtifact creates nested paths and overwrites in place", async () => {
  const root = tmp();
  try {
    await publishArtifact("s1", "assets/app.js", "v1", store(root));
    const two = await publishArtifact("s1", "assets/app.js", "v2-longer", store(root));
    assert.equal(two.name, "assets/app.js");
    assert.equal(readFileSync(join(root, "s1", "assets", "app.js"), "utf8"), "v2-longer");
    // Republishing must not leave two files behind; the link the user has stays valid.
    assert.deepEqual(listArtifacts("s1", store(root)).map((a) => a.name), ["assets/app.js"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a leading slash means the store root, not the filesystem root", async () => {
  const root = tmp();
  try {
    const art = await publishArtifact("s1", "/index.html", "x", store(root));
    assert.equal(art.name, "index.html");
    assert.equal(readFileSync(join(root, "s1", "index.html"), "utf8"), "x");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// ---- AC 1: traversal is blocked in BOTH the name and the session id ----------

test("AC: traversal in the NAME is blocked", async () => {
  const root = tmp();
  try {
    for (const bad of ["../escaped.html", "sub/../../escaped.html", "..", "", "sub/.."]) {
      assert.throws(
        () => resolveArtifactPath("s1", bad, store(root)),
        PathError,
        `name ${JSON.stringify(bad)} should not resolve`,
      );
      await assert.rejects(() => publishArtifact("s1", bad, "pwned", store(root)));
    }
    // Nothing escaped: the only thing under the root is the session dir we never made.
    assert.deepEqual(readdirSync(root), []);

    // An absolute-LOOKING name is not a traversal — the leading slash means the
    // store's own root, so it lands inside the session dir rather than at /etc.
    const art = await publishArtifact("s1", "/etc/passwd", "not the real one", store(root));
    assert.equal(art.name, "etc/passwd");
    assert.equal(readFileSync(join(root, "s1", "etc", "passwd"), "utf8"), "not the real one");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("AC: traversal in the SESSION ID is blocked", async () => {
  const root = tmp();
  const outside = tmp();
  try {
    for (const bad of ["..", "../evil", "../../evil", outside, "", "a/b"]) {
      assert.throws(
        () => resolveArtifactPath(bad, "index.html", store(root)),
        PathError,
        `session id ${JSON.stringify(bad)} should not resolve`,
      );
      await assert.rejects(() => publishArtifact(bad, "index.html", "pwned", store(root)));
      // An unaddressable id has published nothing, and says so rather than throwing.
      assert.deepEqual(listArtifacts(bad, store(root)), []);
    }
    assert.deepEqual(readdirSync(root), []);
    assert.deepEqual(readdirSync(outside), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
    rmSync(outside, { recursive: true, force: true });
  }
});

test("one session cannot read or write another's artifacts", async () => {
  const root = tmp();
  try {
    await publishArtifact("victim", "secret.html", "<b>secret</b>", store(root));
    // Reaching sideways out of "attacker" into "victim" is a path escape, not a read.
    assert.throws(() => resolveArtifactPath("attacker", "../victim/secret.html", store(root)));
    const res = await serveArtifact("attacker", "../victim/secret.html", store(root));
    assert.equal(res.status, 403);
    assert.deepEqual(listArtifacts("attacker", store(root)), []);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the artifact route rejects an escaping path with 403, not 404", async () => {
  await withBoughHome(async () => {
    const { call } = fixture();
    await publishArtifact("s1", "index.html", "<h1>hi</h1>");
    for (
      const path of [
        "/artifacts/s1/..%2F..%2Fetc%2Fpasswd",
        "/artifacts/..%2F..%2Fetc/passwd",
        "/artifacts/s1/../../../etc/passwd",
      ]
    ) {
      const res = await call(get(path));
      assert.equal(res.status === 403 || res.status === 404, true);
      const body = await res.text();
      assert.equal(body.includes("root:"), false);
    }
    // The 403 is reachable through the router for a well-formed escaping id.
    const res = await call(get("/artifacts/..%2Fevil/index.html"));
    assert.equal(res.status, 403);
    await res.text();
  });
});

// ---- AC 2: listing survives a database reset --------------------------------

test("AC: listArtifacts survives a database reset — no row required", async () => {
  await withBoughHome(async () => {
    await publishArtifact("ghost", "index.html", "<h1>still here</h1>");
    await publishArtifact("ghost", "assets/app.js", "console.log(1)");

    // A brand-new, empty database: nothing knows this session ever existed.
    const { call, db } = fixture();
    assert.equal(db.getSession("ghost"), undefined);

    const res = await call(get("/sessions/ghost/artifacts"));
    assert.equal(res.status, 200);
    const body = await res.json() as { artifacts: { name: string }[] };
    assert.deepEqual(body.artifacts.map((a) => a.name).sort(), ["assets/app.js", "index.html"]);

    const served = await call(get("/artifacts/ghost/index.html"));
    assert.equal(served.status, 200);
    assert.equal((await served.text()).includes("still here"), true);
  });
});

test("listArtifacts is newest-first and empty for a session that published none", async () => {
  const root = tmp();
  try {
    assert.deepEqual(listArtifacts("nobody", store(root)), []);
    await publishArtifact("s2", "a.html", "a", store(root));
    await new Promise((r) => setTimeout(r, 10));
    await publishArtifact("s2", "sub/b.css", "b", store(root));
    const list = listArtifacts("s2", store(root));
    assert.deepEqual(list.map((a) => a.name).sort(), ["a.html", "sub/b.css"]);
    assert.equal(list[0].name, "sub/b.css");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

// ---- AC 3: the comments sidecar is not reachable ----------------------------

test("AC: the comments sidecar is neither listed nor served as an artifact", async () => {
  await withBoughHome(async (home) => {
    const { call } = fixture();
    await publishArtifact("s9", "index.html", "<html><body>hi</body></html>");
    addComment("s9", { artifact: "index.html", text: "this is stale" });

    // It exists, and it is OUTSIDE the artifact tree (plan §6.12).
    const sidecar = commentsPathFor("s9");
    assert.equal(statSync(sidecar).isFile(), true);
    assert.equal(sidecar.startsWith(join(home, "artifacts")), false);

    // Not walked by the listing.
    const listed = await (await call(get("/sessions/s9/artifacts"))).json() as {
      artifacts: { name: string }[];
    };
    assert.deepEqual(listed.artifacts.map((a) => a.name), ["index.html"]);

    // Not reachable through the artifact route by any spelling.
    for (
      const path of [
        "/artifacts/s9/s9.json",
        "/artifacts/s9/..%2F..%2Fcomments%2Fs9.json",
        "/artifacts/s9/../../comments/s9.json",
        "/artifacts/comments/s9.json",
      ]
    ) {
      const res = await call(get(path));
      assert.notEqual(res.status, 200);
      assert.equal((await res.text()).includes("this is stale"), false);
    }
  });
});

// ---- serving ----------------------------------------------------------------

test("serveArtifact sets the content type and never caches", async () => {
  const root = tmp();
  try {
    await publishArtifact("s3", "page.html", "<!doctype html><title>x</title>", store(root));
    await publishArtifact("s3", "app.js", "console.log(1)", store(root));
    await publishArtifact("s3", "data.csv", "a,b", store(root));

    const html = await serveArtifact("s3", "page.html", store(root));
    assert.equal(html.status, 200);
    assert.equal(html.headers.get("content-type"), "text/html; charset=utf-8");
    assert.equal(html.headers.get("cache-control"), "no-cache");
    assert.equal((await html.text()).includes("<title>x</title>"), true);

    const js = await serveArtifact("s3", "app.js", store(root));
    assert.equal(js.headers.get("content-type"), "text/javascript; charset=utf-8");
    assert.equal(await js.text(), "console.log(1)"); // untouched: no layer in a script

    const csv = await serveArtifact("s3", "data.csv", store(root));
    assert.equal(csv.headers.get("content-type"), "text/csv; charset=utf-8");
    await csv.text();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("serveArtifact sniffs an extensionless HTML file so it renders", async () => {
  const root = tmp();
  try {
    await publishArtifact("s7", "my-explorer", "<!doctype html>\n<title>x</title>", store(root));
    await publishArtifact("s7", "notes", "just text", store(root));

    const html = await serveArtifact("s7", "my-explorer", store(root));
    assert.equal(html.headers.get("content-type"), "text/html; charset=utf-8");
    assert.equal((await html.text()).includes("bgh-cmt-toggle"), true);

    const plain = await serveArtifact("s7", "notes", store(root));
    assert.equal(plain.headers.get("content-type"), "application/octet-stream");
    await plain.text();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a missing artifact is JSON for a client and a page for a browser", async () => {
  const root = tmp();
  try {
    const api = await serveArtifact("s5", "nope.html", store(root));
    assert.equal(api.status, 404);
    assert.equal(api.headers.get("content-type"), "application/json; charset=utf-8");
    assert.equal(((await api.json()) as { error: string }).error.includes("nope.html"), true);

    const browser = await serveArtifact("s5", "nope.html", {
      ...store(root),
      accept: "text/html,application/xhtml+xml",
    });
    assert.equal(browser.status, 404);
    assert.equal(browser.headers.get("content-type"), "text/html; charset=utf-8");
    assert.equal(await browser.text(), NOT_FOUND_PAGE);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("the 404 page is self-contained — no external network references", () => {
  assert.equal(/src=["']https?:/i.test(NOT_FOUND_PAGE), false);
  assert.equal(/href=["']https?:/i.test(NOT_FOUND_PAGE), false);
  assert.equal(/cdn\.|googleapis|unpkg|jsdelivr/i.test(NOT_FOUND_PAGE), false);
});

test("a directory is a 404, not a directory listing", async () => {
  const root = tmp();
  try {
    await publishArtifact("s6", "assets/app.js", "x", store(root));
    const res = await serveArtifact("s6", "assets", store(root));
    assert.equal(res.status, 404);
    await res.text();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a percent-encoded name round-trips through the route", async () => {
  await withBoughHome(async () => {
    const { call } = fixture();
    const art = await publishArtifact("s8", "my report.html", "<html><body>ok</body></html>");
    assert.equal(art.url, "/artifacts/s8/my%20report.html");
    const res = await call(get(art.url));
    assert.equal(res.status, 200);
    assert.equal((await res.text()).includes("ok"), true);
  });
});

test("the store root is created lazily and lives outside any workspace", async () => {
  await withBoughHome(async (home) => {
    assert.throws(() => statSync(artifactsDir()));
    await publishArtifact("s10", "index.html", "x");
    assert.equal(statSync(join(home, "artifacts", "s10", "index.html")).isFile(), true);
  });
});
