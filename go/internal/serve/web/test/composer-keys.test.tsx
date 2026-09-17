import { expect, mock, test } from "bun:test";
import { readFileSync } from "fs";
import { join } from "path";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { escStops, stoppedPrompt } = await import("../src/app");
import type { Line } from "../src/types";

const at = "2026-01-02T10:00:00Z";
const l = (seq: number, kind: string, text = "") => ({ seq, at, kind, text }) as Line;

test("Esc in the composer stops a running turn unless a picker owns it", () => {
  expect(escStops({ key: "Escape", running: true, pickerOpen: false })).toBe(true);
  expect(escStops({ key: "Escape", running: true, pickerOpen: true })).toBe(false);
  expect(escStops({ key: "Escape", running: false, pickerOpen: false })).toBe(false);
  expect(escStops({ key: "Enter", running: true, pickerOpen: false })).toBe(false);
  expect(escStops({ key: "Escape", running: true, pickerOpen: false, composing: true })).toBe(false);
});

test("a stopped turn with no reply gives its prompt back", () => {
  expect(stoppedPrompt([l(1, "input", "old"), l(2, "done"), l(3, "input", "fix it"), l(4, "code"), l(5, "cancelled")])).toBe("fix it");
  expect(stoppedPrompt([l(1, "input", "fix it"), l(2, "assistant", "Looking"), l(3, "cancelled")])).toBe("");
  expect(stoppedPrompt([l(1, "input", "fix it"), l(2, "done")])).toBe("");
  expect(stoppedPrompt([l(1, "input", "fix it")])).toBe("");
});

test("Esc no longer leaves the session from the app shell", () => {
  const src = readFileSync(join(import.meta.dir, "../src/app.tsx"), "utf8");
  expect(src).not.toContain("Esc leaves a session");
});

// R3-I: a provider 401 offers a way out of the model that cannot answer.
test("a provider auth error offers Switch model; other errors do not", async () => {
  const { renderToStaticMarkup } = await import("react-dom/server");
  const { Entry, authError } = await import("../src/app");
  expect(authError("llm-anthropic: 401 Unauthorized: invalid x-api-key")).toBe(true);
  expect(authError("authentication_error: invalid api key")).toBe(true);
  expect(authError("context deadline exceeded")).toBe(false);
  expect(authError("block 2: SyntaxError at line 401")).toBe(false);
  expect(authError("git push: Authentication failed for github.com")).toBe(false);
  const bad = renderToStaticMarkup(<Entry line={l(3, "error", "anthropic: 401 invalid x-api-key")} codes={[]} />);
  expect(bad).toContain("Switch model");
  const other = renderToStaticMarkup(<Entry line={l(3, "error", "network down")} codes={[]} />);
  expect(other).not.toContain("Switch model");
});
