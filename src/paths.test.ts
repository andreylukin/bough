/**
 * Tests for the `~/.bough` layout and `confine`.
 *
 * Two invariants are under test here. The first is that `BOUGH_HOME` relocates the
 * *whole* tree: every accessor resolves through `boughHome()`, which is what lets
 * the rewrite run beside the live install without touching it (plan §2). A test
 * that had to write to the real `~/.bough` to check the layout would be exactly the
 * bug this guards against, so nothing below touches the filesystem except one
 * symlink case, which uses a temp dir.
 *
 * The second is `confine`'s escape rule. It is checked from both directions —
 * escapes rejected AND legitimate paths that merely *contain* `..` accepted —
 * because a `confine` that rejects everything would pass a one-sided test while
 * breaking every caller.
 *
 * Assertions come from `node:assert` rather than `@std/assert`: jsr.io is denied by
 * this environment's egress policy (`host_not_allowed`), so the jsr import declared
 * in `deno.json` cannot resolve. `node:assert` is built into the runtime and needs
 * no fetch. See the task notes — an environment constraint, not a preference.
 */
import { strictEqual, throws } from "node:assert";
import { isAbsolute, join, resolve } from "node:path";
import { PathError } from "./errors.ts";
import {
  artifactsDir,
  artifactsDirFor,
  attachmentsDir,
  boughHome,
  boughPath,
  commentsDir,
  commentsPathFor,
  confine,
  dbPath,
  envPath,
  logsDir,
  mcpAuthPath,
  mcpRegistryPath,
  themePath,
  userSkillsDir,
  workflowScriptPath,
  workflowsDir,
} from "./paths.ts";

const assertEquals = <T>(actual: T, expected: T, msg?: string): void =>
  strictEqual(actual, expected, msg);

/** Assert `fn` throws a `PathError`, and hand the error back for inspection. */
function assertThrowsPath(fn: () => unknown): PathError {
  let caught: unknown;
  throws(fn, (e: unknown) => {
    caught = e;
    return e instanceof PathError;
  });
  return caught as PathError;
}

/** The escape assertion, where the error object itself is not interesting. */
const assertEscapes = (fn: () => unknown): void => void assertThrowsPath(fn);

/** Run `fn` with env vars set to fixed values, then restore whatever was there. */
function withEnv(vars: Record<string, string | undefined>, fn: () => void): void {
  const prior = new Map<string, string | undefined>();
  for (const [k, v] of Object.entries(vars)) {
    prior.set(k, Deno.env.get(k));
    if (v === undefined) Deno.env.delete(k);
    else Deno.env.set(k, v);
  }
  try {
    fn();
  } finally {
    for (const [k, v] of prior) {
      if (v === undefined) Deno.env.delete(k);
      else Deno.env.set(k, v);
    }
  }
}

// ---- the layout -------------------------------------------------------------

Deno.test("BOUGH_HOME relocates the entire tree", () => {
  withEnv({ BOUGH_HOME: "/fake/root", BOUGH_DB: undefined }, () => {
    assertEquals(boughHome(), "/fake/root");
    assertEquals(boughPath("x", "y"), "/fake/root/x/y");
    assertEquals(dbPath(), "/fake/root/bough.db");
    assertEquals(artifactsDir(), "/fake/root/artifacts");
    assertEquals(artifactsDirFor("s1"), "/fake/root/artifacts/s1");
    assertEquals(commentsDir(), "/fake/root/comments");
    assertEquals(commentsPathFor("s1"), "/fake/root/comments/s1.json");
    assertEquals(attachmentsDir(), "/fake/root/attachments");
    assertEquals(workflowsDir(), "/fake/root/workflows");
    assertEquals(workflowScriptPath("w7"), "/fake/root/workflows/w7.js");
    assertEquals(userSkillsDir(), "/fake/root/skills");
    assertEquals(themePath(), "/fake/root/theme.json");
    assertEquals(envPath(), "/fake/root/env");
    assertEquals(mcpRegistryPath(), "/fake/root/mcp.json");
    assertEquals(mcpAuthPath(), "/fake/root/mcp-auth.json");
    assertEquals(logsDir(), "/fake/root/logs");
  });
});

Deno.test("an unset or blank BOUGH_HOME falls back to ~/.bough", () => {
  // A blank override is a shell accident (`BOUGH_HOME= bough`), not a request to
  // put the data root at the filesystem root or the cwd.
  for (const v of [undefined, "", "   "]) {
    withEnv({ BOUGH_HOME: v }, () => {
      const home = boughHome();
      assertEquals(home.endsWith("/.bough"), true, home);
      assertEquals(isAbsolute(home), true, home);
    });
  }
});

Deno.test("BOUGH_DB overrides the database path outright, including :memory:", () => {
  withEnv({ BOUGH_HOME: "/fake/root", BOUGH_DB: ":memory:" }, () => {
    assertEquals(dbPath(), ":memory:");
  });
});

Deno.test("comment sidecars live outside the artifacts tree", () => {
  // Invariant §6.12: a sidecar under artifacts/ would be walked by every listing
  // and served as an artifact itself.
  withEnv({ BOUGH_HOME: "/fake/root" }, () => {
    const artifacts = artifactsDir();
    assertEquals(commentsDir().startsWith(artifacts + "/"), false);
    assertEquals(commentsPathFor("s1").startsWith(artifacts + "/"), false);
    assertEscapes(() => confine(artifacts, commentsPathFor("s1")));
  });
});

// ---- confine: the accepting direction ---------------------------------------

Deno.test("confine returns an absolute path under the root", () => {
  assertEquals(confine("/a/b", "c"), "/a/b/c");
  assertEquals(confine("/a/b", "c/d/e.html"), "/a/b/c/d/e.html");
  assertEquals(confine("/a/b", "./c"), "/a/b/c");
});

Deno.test("confine accepts a candidate that contains .. but lands back inside", () => {
  // The check is on the RESOLVED path, not on the presence of a ".." segment —
  // rejecting the substring would break legitimate callers.
  assertEquals(confine("/a/b", "c/../d"), "/a/b/d");
  assertEquals(confine("/a/b", "c/d/../../e"), "/a/b/e");
});

Deno.test("confine accepts an absolute candidate that is already inside the root", () => {
  assertEquals(confine("/a/b", "/a/b/c"), "/a/b/c");
  assertEquals(confine("/a/b", "/a/b"), "/a/b");
});

Deno.test("confine normalizes the root, and an empty candidate is the root itself", () => {
  assertEquals(confine("/a/b/", "c"), "/a/b/c");
  assertEquals(confine("/a/b//", "c"), "/a/b/c");
  assertEquals(confine("/a/./b", "c"), "/a/b/c");
  assertEquals(confine("/a/b", ""), "/a/b");
  assertEquals(confine("/a/b", "."), "/a/b");
});

Deno.test("confine resolves a relative root against the cwd", () => {
  assertEquals(confine("store", "x"), resolve(Deno.cwd(), "store/x"));
});

Deno.test("confine handles the filesystem root without a doubled separator", () => {
  assertEquals(confine("/", "etc"), "/etc");
  assertEquals(confine("/", "/etc"), "/etc");
  assertEquals(confine("/", ".."), "/"); // "/.." is "/" — inside, not an escape
});

// ---- confine: ".." traversal ------------------------------------------------

Deno.test("confine rejects .. traversal out of the root", () => {
  assertEscapes(() => confine("/a/b", ".."));
  assertEscapes(() => confine("/a/b", "../c"));
  assertEscapes(() => confine("/a/b", "../../etc/passwd"));
  assertEscapes(() => confine("/a/b", "c/../../d"));
  // Landing exactly on the parent of the root is still outside it.
  assertEscapes(() => confine("/a/b", "../"));
});

Deno.test("confine rejects a chain whose segments each look harmless", () => {
  // Every segment here is a plain name; only the resolved path escapes.
  assertEscapes(() => confine("/a/b", "x/y/z/../../../../etc/passwd"));
});

Deno.test("confine's error names the candidate, where it landed, and the root", () => {
  // Error text is a product surface: the message must say what failed, the state
  // that caused it, and the move that resolves it.
  const err = assertThrowsPath(() => confine("/a/b", "../../etc/passwd"));
  assertEquals(err.message.includes("../../etc/passwd"), true, err.message);
  assertEquals(err.message.includes("/etc/passwd"), true, err.message);
  assertEquals(err.message.includes("/a/b"), true, err.message);
  assertEquals(err.status, 400);
});

// ---- confine: absolute escapes ----------------------------------------------

Deno.test("confine rejects an absolute candidate outside the root", () => {
  assertEscapes(() => confine("/a/b", "/etc/passwd"));
  assertEscapes(() => confine("/a/b", "/"));
  assertEscapes(() => confine("/a/b", "/a"));
});

Deno.test("confine rejects a sibling that merely shares a string prefix", () => {
  // "/a/bc" starts with "/a/b" as a STRING but is not under it as a PATH.
  assertEscapes(() => confine("/a/b", "/a/bc"));
  assertEscapes(() => confine("/a/b", "/a/bc/d"));
  assertEscapes(() => confine("/a/b", "../bc/d"));
});

Deno.test("confine rejects a NUL byte rather than letting it truncate a path", () => {
  assertEscapes(() => confine("/a/b", "ok\0/../../etc/passwd"));
  assertEscapes(() => confine("/a/b\0", "c"));
});

Deno.test("a session id with traversal cannot steer the artifact directory", () => {
  // The shape of the real caller: a session id arriving in a URL.
  withEnv({ BOUGH_HOME: "/fake/root" }, () => {
    assertEquals(confine(artifactsDir(), artifactsDirFor("s1")), "/fake/root/artifacts/s1");
    assertEscapes(() => confine(artifactsDir(), artifactsDirFor("../../etc")));
    assertEscapes(() => confine(artifactsDirFor("s1"), "../s2/secret.html"));
  });
});

// ---- confine: symlink-shaped inputs -----------------------------------------

Deno.test("confine rejects traversal that routes through a symlinked directory", async () => {
  const tmp = await Deno.makeTempDir({ prefix: "bough-paths-" });
  try {
    const root = join(tmp, "root");
    const outside = join(tmp, "outside");
    await Deno.mkdir(root);
    await Deno.mkdir(outside);
    await Deno.writeTextFile(join(outside, "secret.txt"), "no");
    // A real symlink inside the root pointing at a directory outside it.
    await Deno.symlink(outside, join(root, "link"));

    // Lexical resolution collapses "link/.." to the root, so a traversal that
    // routes through the link still resolves outward and is rejected.
    assertEscapes(() => confine(root, "link/../../outside/secret.txt"));
    assertEscapes(() => confine(root, "link/../.."));

    // Documented boundary: the link itself resolves inside the root and is
    // ACCEPTED, because confine is lexical and never follows symlinks. Following
    // it is not confinement's job and cannot be — programs already read any path
    // they like with the user's authority (spec §2). Asserting it pins the
    // contract, so a later move to filesystem-based resolution is a deliberate
    // decision rather than a silent one.
    assertEquals(confine(root, "link"), join(root, "link"));
    assertEquals(confine(root, "link/secret.txt"), join(root, "link/secret.txt"));
  } finally {
    await Deno.remove(tmp, { recursive: true });
  }
});

Deno.test("confine treats a symlinked root and its realpath as different namespaces", () => {
  // The macOS /tmp -> /private/tmp shape. A candidate that has been through
  // realpath no longer matches a root that has not, so callers must build both
  // from the same source — which is why every root comes from boughPath().
  assertEscapes(() => confine("/tmp/store", "/private/tmp/store/a.html"));
  assertEquals(
    confine("/private/tmp/store", "/private/tmp/store/a.html"),
    "/private/tmp/store/a.html",
  );
});
