import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { COLOR, fuzzyScore, md, wordLeft, wordRight } from "./format.ts";

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

Deno.test("md: bare URLs become clickable, trailing punctuation stays prose", () => {
  if (!COLOR) return;
  const out = md("try https://example.com/a.");
  assertStringIncludes(out, `${LINK_OPEN("https://example.com/a")}https://example.com/a${LINK_CLOSE}.`);
});

Deno.test("md: URLs inside code spans are not linkified", () => {
  if (!COLOR) return;
  const out = md("run `curl https://example.com`");
  assertEquals(out.includes("]8;;"), false);
});
