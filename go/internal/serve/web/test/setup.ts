import { mock } from "bun:test";

// DOMPurify needs a DOM, and bun's tests have none: its default export
// then has no sanitize. Mocks are global to the run, so a file that
// rendered markdown without mocking it passed only when an earlier file
// happened to mock it, and failed when the order changed (CI, 2026-09-24).
// Markdown sanitising is not what these tests assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
