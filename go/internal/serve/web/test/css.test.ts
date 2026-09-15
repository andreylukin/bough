import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");

test("the composer primary stays accent-filled, disabled included", () => {
  const rules = [...css.matchAll(/([^{}]*\.btn-primary[^{}]*)\{([^}]*)\}/g)].filter(([, sel]) => /\.composer/.test(sel));
  expect(rules.length).toBeGreaterThan(0);
  for (const [, , body] of rules) expect(body).not.toMatch(/background:var\(--(surface|raised|bg)\)/);
});
