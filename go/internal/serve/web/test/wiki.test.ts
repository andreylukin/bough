import { expect, test } from "bun:test";
import { humanError } from "../src/loading";
import { cleanExcerpt, countsLine, denumber, humanTitle, literalUnderscores, stamp } from "../src/wiki";

test("literalUnderscores escapes prose underscores and leaves code alone", () => {
  expect(literalUnderscores("set max_retry and _init_ in `a_b`")).toBe("set max\\_retry and \\_init\\_ in `a_b`");
  expect(literalUnderscores("```\nx_y\n```")).toBe("```\nx_y\n```");
});

test("denumber strips the editor gutter and the empty Sessions list", () => {
  expect(denumber("1|# Ports\n2|\n3|- a claim\nUpdated: 2026-09-11 · Sessions:,,,,,")).toBe("# Ports\n\n- a claim\nUpdated: 2026-09-11");
  expect(denumber("Sessions:,,,,")).toBe("");
});

test("countsLine counts claims, not chips", () => {
  expect(countsLine({ cited: 2, inferred: 1, uncited: 0, unsupported: 0, superseded: 0 })).toBe("2 cited claims · 1 inferred");
});

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
