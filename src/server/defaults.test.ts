import assert from "node:assert/strict";
import { test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadDefaults, NO_DEFAULTS, saveDefaults } from "./defaults.ts";

// Every test injects its own path. Nothing here may touch a real `~/.bough` — a
// test that wrote the developer's home directory would change their editor's model.
const scratch = () => join(mkdtempSync(join(tmpdir(), "bough-defaults-")), "model.json");

test("nothing stored is the ordinary state, not a failure", () => {
  assert.deepEqual(loadDefaults(scratch()), NO_DEFAULTS);
});

test("a saved default round-trips", () => {
  const path = scratch();
  saveDefaults({ model: "claude-sonnet-5", effort: "high" }, path);
  assert.deepEqual(loadDefaults(path), { model: "claude-sonnet-5", effort: "high" });
});

test("null clears a pin — 'let the provider decide' is a real state", () => {
  const path = scratch();
  saveDefaults({ model: "claude-sonnet-5", effort: "high" }, path);
  saveDefaults({ model: "claude-sonnet-5", effort: null }, path);
  assert.deepEqual(loadDefaults(path), { model: "claude-sonnet-5", effort: null });
});

test("a hand-edited file degrades to unpinned rather than throwing", () => {
  // This runs on the path that answers WHICH MODEL TO USE. Taking the server down
  // because someone fat-fingered the JSON would be much worse than falling back.
  for (const bad of ["{ not json", "null", "[]", '"a string"', '{"model": 42}']) {
    const path = scratch();
    writeFileSync(path, bad);
    assert.deepEqual(loadDefaults(path), NO_DEFAULTS, bad);
  }
});

test("an unknown effort is dropped, and the model beside it survives", () => {
  const path = scratch();
  writeFileSync(path, JSON.stringify({ model: "claude-opus-5", effort: "turbo" }));
  assert.deepEqual(loadDefaults(path), { model: "claude-opus-5", effort: null });
});

test("blank and whitespace-only models read as unpinned", () => {
  // "" is what an empty picker row would send; it must not become a pin on the
  // empty string, which would resolve to no model at all at turn time.
  const path = scratch();
  writeFileSync(path, JSON.stringify({ model: "   ", effort: "low" }));
  assert.deepEqual(loadDefaults(path), { model: null, effort: "low" });
});

test("save rebuilds the document — it trims, and extra keys do not ride along", () => {
  const path = scratch();
  saveDefaults({ model: " claude-opus-5 ", effort: "max", secret: "x" } as never, path);
  const stored = JSON.parse(readFileSync(path, "utf8")) as Record<string, unknown>;
  assert.deepEqual(Object.keys(stored).sort(), ["effort", "model"]);
  assert.equal(stored.model, "claude-opus-5");
});
