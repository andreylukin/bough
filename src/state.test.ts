import { assertEquals, assertThrows } from "jsr:@std/assert@1";
import { openDb } from "./db/db.ts";
import { stateVerb } from "./state.ts";

function db() {
  return openDb(":memory:");
}

Deno.test("state: set/get/list/delete round-trip", () => {
  const d = db();
  assertEquals(stateVerb(d, "root", "get", "todo"), null);
  stateVerb(d, "root", "set", { key: "todo", value: { left: ["a.ts", "b.ts"] } });
  assertEquals(stateVerb(d, "root", "get", "todo"), { left: ["a.ts", "b.ts"] });
  const list = stateVerb(d, "root", "list", null) as { key: string }[];
  assertEquals(list.map((r) => r.key), ["todo"]);
  assertEquals((stateVerb(d, "root", "delete", "todo") as { removed: boolean }).removed, true);
  assertEquals((stateVerb(d, "root", "delete", "todo") as { removed: boolean }).removed, false);
  assertEquals(stateVerb(d, "root", "get", "todo"), null);
});

Deno.test("state: lineages are isolated; re-set overwrites", () => {
  const d = db();
  stateVerb(d, "a", "set", { key: "k", value: 1 });
  stateVerb(d, "b", "set", { key: "k", value: 2 });
  stateVerb(d, "a", "set", { key: "k", value: 3 });
  assertEquals(stateVerb(d, "a", "get", "k"), 3);
  assertEquals(stateVerb(d, "b", "get", "k"), 2);
  assertEquals((stateVerb(d, "a", "list", null) as unknown[]).length, 1);
});

Deno.test("state: bad args and oversized values are catchable errors", () => {
  const d = db();
  assertThrows(() => stateVerb(d, "root", "get", ""), Error, "key");
  assertThrows(() => stateVerb(d, "root", "set", { key: "k" }), Error, "value required");
  assertThrows(
    () => stateVerb(d, "root", "set", { key: "k", value: "x".repeat(20_000) }),
    Error,
    "too large",
  );
  assertThrows(() => stateVerb(d, "root", "nope", null), Error, "unknown state verb");
});
