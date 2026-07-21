import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import {
  COLOR,
  disconnectNote,
  fuzzyPositions,
  fuzzyScore,
  linkAt,
  md,
  sessionLabel,
  wordLeft,
  wordRight,
} from "./format.ts";

Deno.test("wordLeft/wordRight: readline word boundaries", () => {
  const t = "what does this do";
  assertEquals(wordLeft(t, t.length), 15); // → start of "do"
  assertEquals(wordLeft(t, 15), 10); // → start of "this"
  assertEquals(wordLeft(t, 0), 0); // clamps
  assertEquals(wordRight(t, 0), 4); // → end of "what"
  assertEquals(wordRight(t, 4), 9); // skips the space, ends "does"
  assertEquals(wordRight(t, t.length), t.length); // clamps
});

Deno.test("wordLeft: whitespace runs collapse into the jump", () => {
  assertEquals(wordLeft("ab   cd", 5), 0); // from inside the gap → start of "ab"
});

Deno.test("fuzzyScore: prefix > word-boundary > substring > subsequence > none", () => {
  assertEquals(fuzzyScore("exa", "ex"), 4);
  assertEquals(fuzzyScore("user-testing", "test"), 3);
  assertEquals(fuzzyScore("restish", "tish"), 2);
  assertEquals(fuzzyScore("wiki", "wk"), 1); // in-order subsequence
  assertEquals(fuzzyScore("commit", "xyz"), 0);
  assertEquals(fuzzyScore("anything", ""), 1); // empty query matches everything
});

Deno.test("fuzzyPositions: marks the chars fuzzyScore matched, tier by tier", () => {
  assertEquals(fuzzyPositions("exa", "ex"), [0, 1]); // prefix
  assertEquals(fuzzyPositions("user-testing", "test"), [5, 6, 7, 8]); // word boundary
  assertEquals(fuzzyPositions("restish", "tish"), [3, 4, 5, 6]); // substring
  assertEquals(fuzzyPositions("wiki", "wk"), [0, 2]); // greedy subsequence
  assertEquals(fuzzyPositions("bench/server.sh", "server"), [6, 7, 8, 9, 10, 11]);
  assertEquals(fuzzyPositions("commit", "xyz"), []); // no match
  assertEquals(fuzzyPositions("anything", ""), []); // empty query: nothing to mark
});

// OSC 8 assertions only make sense when escapes are emitted (COLOR honors NO_COLOR).
const LINK_OPEN = (url: string) => `\x1b]8;;${url}\x1b\\`;
const LINK_CLOSE = "\x1b]8;;\x1b\\";

Deno.test("md: markdown links wrap in an OSC 8 hyperlink", () => {
  if (!COLOR) return;
  const out = md("see [the docs](https://example.com/x)");
  assertStringIncludes(out, LINK_OPEN("https://example.com/x"));
  assertStringIncludes(out, LINK_CLOSE);
  // One link: the dimmed (url) alongside must not be re-linkified by the bare-URL pass.
  assertEquals(out.split("]8;;").length - 1, 2); // one open + one close
});

Deno.test("md: a link whose label IS the url renders without the parenthetical", () => {
  if (!COLOR) return;
  const out = md("see [https://git-scm.com](https://git-scm.com)");
  assertStringIncludes(out, LINK_OPEN("https://git-scm.com"));
  assertEquals(out.includes("(https://git-scm.com)"), false); // no "url (url)" dup
});

Deno.test("md: bare URLs become clickable and underlined, trailing punctuation stays prose", () => {
  if (!COLOR) return;
  const out = md("try https://example.com/a.");
  // Underlined like rendered [text](url) links, so a pasted URL reads as a link.
  assertStringIncludes(
    out,
    `${LINK_OPEN("https://example.com/a")}\x1b[4mhttps://example.com/a\x1b[24m${LINK_CLOSE}.`,
  );
});

Deno.test("md: fenced code sits on a raised surface when a width is given", async () => {
  if (!COLOR) return;
  const { surface } = await import("./format.ts");
  const out = md("```js\nconst x = 1\n```", 40);
  assertStringIncludes(out, "\x1b[48;2;"); // truecolor bg behind the block
  assertEquals(md("plain prose", 40).includes("\x1b[48;2;"), false); // prose stays bare
  // surface() pads to the width and closes the bg at the end of the line.
  const line = surface("hi", 10);
  assertEquals(line.endsWith(" ".repeat(8) + "\x1b[0m"), true);
});

Deno.test("linkifyUrls: raw output lines get clickable OSC 8 URLs", async () => {
  if (!COLOR) return;
  const { linkifyUrls } = await import("./format.ts");
  const out = linkifyUrls("artifact published: http://localhost:4321/artifacts/s1/report.html");
  assertStringIncludes(out, LINK_OPEN("http://localhost:4321/artifacts/s1/report.html"));
  assertStringIncludes(out, "\x1b[4mhttp://localhost:4321/artifacts/s1/report.html\x1b[24m");
  assertEquals(linkifyUrls("no links here"), "no links here");
});

Deno.test("md: URLs inside code spans are not linkified", () => {
  if (!COLOR) return;
  const out = md("run `curl https://example.com`");
  assertEquals(out.includes("]8;;"), false);
});

Deno.test("linkAt: resolves the hyperlink under a display column", () => {
  if (!COLOR) return;
  // "go " + linked "docs (url)" + " end", with bold prose before the link.
  const line = md("go **now** [docs](https://example.com/x) end");
  // Plain text: "go now docs (https://example.com/x) end" — link spans cols 7..35.
  assertEquals(linkAt(line, 0), null); // "g"
  assertEquals(linkAt(line, 7), "https://example.com/x"); // "d" of docs
  assertEquals(linkAt(line, 34), "https://example.com/x"); // inside the dimmed url
  assertEquals(linkAt(line, 37), null); // "e" of end
  assertEquals(linkAt(line, 200), null); // past end of line
});

Deno.test("linkAt: wrapped continuation fragment still carries the full target", () => {
  if (!COLOR) return;
  // wrap-ansi re-opens the link on a continuation line that never sees a close
  // in-slice order: open + text, close at the very end.
  const frag = "\x1b]8;;https://example.com/long\x1b\\example.com/lo\x1b]8;;\x1b\\";
  assertEquals(linkAt(frag, 5), "https://example.com/long");
  assertEquals(linkAt(frag, 14), null); // one past the visible text
});

Deno.test("coldCacheNote: fires only for stale, substantial contexts", async () => {
  const { coldCacheNote } = await import("./format.ts");
  const now = 1_000_000_000;
  const warm = { contextTokens: 180_000, lastLlmAt: now - 60_000 };
  const stale = { contextTokens: 180_000, lastLlmAt: now - 6 * 60_000 };
  const small = { contextTokens: 5_000, lastLlmAt: now - 6 * 60_000 };
  assertEquals(coldCacheNote(warm, now), null);
  assertEquals(coldCacheNote(stale, now), "❄ re-caches ~180k");
  assertEquals(coldCacheNote(small, now), null); // trivial cost — no noise
  assertEquals(coldCacheNote({ contextTokens: 180_000, lastLlmAt: null }, now), null);
  assertEquals(coldCacheNote({ contextTokens: 180_000 }, now), null); // never ran
});

Deno.test("sessionLabel: title wins; untitled falls back to the workspace basename", () => {
  assertEquals(sessionLabel("Fix the bug", "/Users/x/repos/ws"), "Fix the bug");
  assertEquals(sessionLabel("untitled", "/Users/x/repos/ws"), "ws"); // server placeholder
  assertEquals(sessionLabel("", "/Users/x/repos/ws/"), "ws"); // trailing slash
  assertEquals(sessionLabel(undefined, null), "(untitled)");
  assertEquals(sessionLabel("  ", ""), "(untitled)");
});

Deno.test("sessionLabel: uuid-named shadow worktrees never leak as labels", () => {
  // A fork inherits its origin's worktree (~/.bough/workspaces/<origin-id>)
  // until its own first turn — the basename is ANOTHER session's uuid.
  assertEquals(
    sessionLabel("untitled", "/Users/x/.bough/workspaces/d54a0527-0c52-4326-a539-32de20780c13"),
    "(untitled)",
  );
});

Deno.test("disconnectNote: quiet blip first, then escalates with elapsed seconds", () => {
  const t0 = 1_000_000;
  assertEquals(disconnectNote(t0, t0 + 5_000), { text: "reconnecting…", urgent: false });
  const late = disconnectNote(t0, t0 + 42_000);
  assertEquals(late.urgent, true);
  assertStringIncludes(late.text, "server unreachable for 42s");
  assertStringIncludes(late.text, "restart it and this will reconnect");
});
