import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ErrorNote, humanError } from "../src/loading";
import { cleanExcerpt, countsLine, denumber, healthTone, humanTitle, literalUnderscores, pageMissing, parseWikiHash, shortRef, stamp } from "../src/wiki";

test("shortRef keeps the shortest token that still names an external cite", () => {
  // Past the chip width the head goes, never the number.
  expect(shortRef("gh", "asi/uni-network-evaluation-scheduler#7801")).toBe("…k-evaluation-scheduler#7801");
  expect(shortRef("gh", "asi/uni-nes#7801")).toBe("uni-nes#7801");
  expect(shortRef("git", "uni-network-evaluation-scheduler@a1b2c3d")).toBe("git:a1b2c3d");
  expect(shortRef("git", "repo@0123456789abcdef0123")).toBe("git:0123456");
  expect(shortRef("linear", "NME-1462")).toBe("NME-1462");
  expect(shortRef("jira", "ABC-1")).toBe("jira:ABC-1");
});

test("healthTone is the worst state on the index", () => {
  expect(healthTone({ installed: true }, 0, 0)).toBe("is-on");
  expect(healthTone({ installed: false }, 0, 0)).toBe("is-waiting");
  expect(healthTone({ installed: true }, 2, 0)).toBe("is-attn");
  expect(healthTone({ installed: true }, 0, 1)).toBe("is-attn");
});

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
  expect(humanTitle("retry-budget", "topics/x/retry-budget.md")).toBe("Retry budget");
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

test("parseWikiHash accepts a page path without .md", () => {
  expect(parseWikiHash("wiki/p/topics/x/retry")).toEqual({ at: "page", path: "topics/x/retry.md" });
  expect(parseWikiHash("wiki/p/topics/x/retry.md")).toEqual({ at: "page", path: "topics/x/retry.md" });
});

test("a bad page path is a missing page, not a retryable error", () => {
  expect(pageMissing("wiki: not a page path")).toBe(true);
  expect(pageMissing("wiki: not found")).toBe(true);
  expect(pageMissing("Failed to fetch")).toBe(false);
});

test("ErrorNote puts the raw error behind a disclosure with one action", () => {
  const html = renderToStaticMarkup(<ErrorNote title="Page not found" err="wiki: not a page path" action={{ label: "Back to wiki", onClick: () => {} }} />);
  expect(html).toContain("Page not found");
  expect(html).toContain("<details");
  expect(html).toContain("wiki: not a page path");
  expect(html.match(/<button/g)?.length).toBe(1);
});
