import { expect, test } from "bun:test";
import { humanError } from "../src/loading";
import { cleanExcerpt, humanTitle, stamp } from "../src/wiki";

test("humanTitle humanizes slug-only titles and keeps real ones", () => {
  expect(humanTitle("nased-demand", "topics/x/nased-demand.md")).toBe("Nased demand");
  expect(humanTitle("", "topics/x/sample-topic.md")).toBe("Sample topic");
  expect(humanTitle("Read the gate, don't tail it", "topics/x/read-the-gate.md")).toBe("Read the gate, don't tail it");
});

test("cleanExcerpt unescapes newlines and drops orphan fences", () => {
  expect(cleanExcerpt("```go\\nok 1\\n```")).toBe("ok 1");
  expect(cleanExcerpt("a\\nb")).toBe("a\nb");
});

test("humanError turns not-found into a sentence", () => {
  expect(humanError(new Error("wiki: not found"))).not.toContain("wiki:");
});

test("stamp formats a bare date without a time", () => {
  expect(stamp("2026-09-11")).not.toContain(":");
});
