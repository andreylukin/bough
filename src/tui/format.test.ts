import { assertEquals } from "jsr:@std/assert@1";
import { wordLeft, wordRight } from "./format.ts";

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
