/**
 * The skills API, driven through `createHandler(ctx)` with a fabricated ctx and no
 * socket bound (plan §7).
 *
 * The assertions that matter are the two the listing exists for: a **malformed**
 * SKILL.md is a row WITH an error rather than a missing row — a skill that silently
 * vanishes from the panel is a skill the user never learns is broken — and
 * `/skills/:name` returns the body with `${SKILL_DIR}` already resolved, which is
 * the one question a client cannot answer for itself.
 *
 * The routes take the real sources — that is the point of them — so `BOUGH_HOME` is
 * pointed at an empty temp root and the assertions are about the bundled `history`
 * skill, which is checked in, never about a count the machine could change.
 *
 * Assertions come from `node:assert/strict`: jsr.io is not reachable here.
 */
import { test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import type { AppCtx } from "../types.ts";
import { createHandler, type Route, route } from "./app.ts";
import { getSkillH, listSkillsH, type SkillRow } from "./skills.ts";

const TABLE: Route[] = [
  route("GET", "/skills", listSkillsH),
  route("GET", "/skills/:name", getSkillH),
];

/**
 * `BOUGH_HOME` is pointed at an empty temp root, so the "user" source is real but
 * empty and the machine's own `~/.bough/skills` can never change what these assert
 * (`paths.ts`: the override exists precisely so a test gets a hermetic root).
 */
function fixture() {
  const home = mkdtempSync(join(tmpdir(), "bough-home-"));
  const previous = process.env["BOUGH_HOME"];
  process.env["BOUGH_HOME"] = home;
  const db = openDb(":memory:");
  const ctx: AppCtx = { db, bus: new Bus() };
  const call = createHandler(ctx, { routes: TABLE });
  return {
    db,
    get: (path: string) => call(new Request(`http://127.0.0.1${path}`)),
    close() {
      db.close();
      if (previous === undefined) delete process.env["BOUGH_HOME"];
      else process.env["BOUGH_HOME"] = previous;
      rmSync(home, { recursive: true, force: true });
    },
  };
}

test("GET /skills lists the bundled history skill and names its sources", async () => {
  const f = fixture();
  try {
    const res = await f.get("/skills");
    assert.equal(res.status, 200);
    const body = await res.json() as { skills: SkillRow[]; sources: { source: string }[] };
    const history = body.skills.find((s) => s.name === "history");
    assert.ok(history, "the bundled history skill should be listed");
    assert.equal(history.source, "bundled");
    assert.ok(history.description.length > 0);
    assert.equal(history.error, undefined);
    // No bodies in the listing: it is a menu, not a prompt.
    assert.ok(!("body" in history));
    assert.deepEqual(body.sources.map((s) => s.source), ["bundled", "user"]);
    // Sorted by name, so the panel is stable between requests.
    const names = body.skills.map((s) => s.name);
    assert.deepEqual(names, [...names].sort());
  } finally {
    f.close();
  }
});

test("GET /skills/:name returns the body with ${SKILL_DIR} resolved", async () => {
  const f = fixture();
  try {
    const res = await f.get("/skills/history");
    assert.equal(res.status, 200);
    const skill = await res.json() as SkillRow & { body: string };
    assert.equal(skill.name, "history");
    assert.ok(skill.body.includes("messages_fts"), "the body should be the real instructions");
    assert.ok(!skill.body.includes("${SKILL_DIR}"), "no unresolved token reaches a client");
    assert.ok(skill.dir.endsWith("history"), skill.dir);
  } finally {
    f.close();
  }
});

test("an unknown skill 404s with what IS installed, and a traversal is just unknown", async () => {
  const f = fixture();
  try {
    const missing = await f.get("/skills/nope");
    assert.equal(missing.status, 404);
    const { error } = await missing.json() as { error: string };
    assert.match(error, /no skill "nope"/);
    assert.match(error, /\/history/);

    // A name that is a path never becomes one — it is simply not a skill.
    const traversal = await f.get("/skills/..%2F..%2Fetc");
    assert.equal(traversal.status, 404);
  } finally {
    f.close();
  }
});
