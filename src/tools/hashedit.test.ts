import { assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@1";
import {
  applyOps,
  checkOps,
  joinLines,
  lineMap,
  parsePatch,
  rebaseOps,
  renderNumbered,
  tagOf,
  toLines,
} from "./hashedit.ts";

const SRC = `function add(a, b) {
  return a + b;
}

function sub(a, b) {
  return a - b;
}
`;

Deno.test("tagOf is stable, four hex, and blind to CRLF/BOM", () => {
  const t = tagOf(SRC);
  assertEquals(/^[0-9A-F]{4}$/.test(t), true);
  assertEquals(tagOf(SRC), t);
  assertEquals(tagOf(SRC.replace(/\n/g, "\r\n")), t);
  assertEquals(tagOf("﻿" + SRC), t);
  // Different content must (practically always) differ.
  assertEquals(tagOf(SRC.replace("a + b", "a * b")) === t, false);
});

Deno.test("renderNumbered emits the header and 1-based right-aligned lines", () => {
  const out = renderNumbered("m.ts", SRC);
  assertStringIncludes(out, `[m.ts#${tagOf(SRC)}]`);
  assertStringIncludes(out, "1:function add(a, b) {");
  assertStringIncludes(out, "2:  return a + b;");
  // No trailing phantom line for the final newline.
  assertEquals(out.trimEnd().endsWith("7:}"), true);
});

Deno.test("parsePatch reads every operation and its body", () => {
  const [sec] = parsePatch(`[m.ts#AB12]
SWAP 2.=2:
+  return a + b + 1;
DEL 4.=4
INS.POST 6:
+// after
INS.HEAD:
+// top
`);
  assertEquals(sec.path, "m.ts");
  assertEquals(sec.tag, "AB12");
  assertEquals(sec.ops.length, 4);
  assertEquals(sec.ops[0], { kind: "swap", a: 2, b: 2, body: ["  return a + b + 1;"] });
  assertEquals(sec.ops[1], { kind: "del", a: 4, b: 4, body: [] });
  assertEquals(sec.ops[2].kind, "ins_post");
  assertEquals(sec.ops[3].body, ["// top"]);
});

Deno.test("parsePatch tolerates the range spellings models actually emit", () => {
  for (const header of ["SWAP 2.=3:", "SWAP 2..3:", "SWAP 2-3:", "SWAP 2 3:"]) {
    const [sec] = parsePatch(`[m.ts#AB12]\n${header}\n+x`);
    assertEquals([sec.ops[0].a, sec.ops[0].b], [2, 3], header);
  }
  // Single-line shorthand and a missing colon.
  assertEquals(parsePatch(`[m.ts#AB12]\nSWAP 5\n+x`)[0].ops[0].b, 5);
  // A `+` alone is a blank line, not a dropped row.
  assertEquals(parsePatch(`[m.ts#AB12]\nSWAP 1:\n+\n+x`)[0].ops[0].body, ["", "x"]);
});

Deno.test("parsePatch rejects malformed input with a corrective message", () => {
  assertThrows(() => parsePatch("SWAP 1:\n+x"), Error, "section header");
  assertThrows(() => parsePatch(`[m.ts#AB12]\n- old line`), Error, '"-" rows are not');
  assertThrows(() => parsePatch(`[m.ts#AB12]\n@@ -1,2 +1,2 @@`), Error, "not an operation");
  assertThrows(() => parsePatch(""), Error, "empty patch");
  assertThrows(() => parsePatch(`[m.ts#AB12]`), Error, "no operations");
  assertThrows(() => parsePatch("+orphan"), Error, "no operation above it");
});

Deno.test("checkOps catches out-of-range anchors and overlapping ops", () => {
  assertThrows(() => checkOps([{ kind: "swap", a: 99, b: 99, body: ["x"] }], 7), Error, "range");
  assertThrows(
    () =>
      checkOps([
        { kind: "swap", a: 1, b: 3, body: ["x"] },
        { kind: "del", a: 3, b: 4, body: [] },
      ], 7),
    Error,
    "overlap",
  );
  assertThrows(() => checkOps([{ kind: "swap", a: 1, b: 1, body: [] }], 7), Error, "no body rows");
});

Deno.test("applyOps runs bottom-up so anchors stay in base coordinates", () => {
  const lines = toLines(SRC);
  const out = applyOps(lines, [
    { kind: "swap", a: 2, b: 2, body: ["  return a + b + 1;"] },
    { kind: "ins_post", a: 7, b: 7, body: ["// end"] },
    { kind: "del", a: 4, b: 4, body: [] },
  ]);
  assertEquals(out[1], "  return a + b + 1;");
  // The DEL of the blank line 4 shifts nothing the other ops depended on.
  assertEquals(out.includes(""), false);
  assertEquals(out[out.length - 1], "// end");
});

Deno.test("joinLines preserves CRLF and the trailing-newline choice", () => {
  assertEquals(joinLines(["a", "b"], "x\ny\n"), "a\nb\n");
  assertEquals(joinLines(["a", "b"], "x\r\ny\r\n"), "a\r\nb\r\n");
  assertEquals(joinLines(["a", "b"], "x\ny"), "a\nb");
});

Deno.test("lineMap tracks lines across an unrelated concurrent edit", () => {
  const base = toLines(SRC);
  const cur = toLines(SRC.replace("  return a - b;", "  return a - b - 1;"));
  const map = lineMap(base, cur);
  assertEquals(map[0], 0); // untouched above
  assertEquals(map[1], 1); // the line we care about
  assertEquals(map[5], null); // the line they rewrote
  assertEquals(map[6], 6); // untouched below
});

Deno.test("PARALLEL: a stale patch rebases when the other write missed its lines", () => {
  // Agent A read SRC. Agent B rewrote sub() and inserted a line above it, which
  // shifts A's target line numbers. A's edit must still land, on the right line.
  const base = toLines(SRC);
  const cur = toLines(
    SRC.replace("function sub", "// B was here\nfunction sub")
      .replace("  return a - b;", "  return a - b - 1;"),
  );
  const res = rebaseOps([{ kind: "swap", a: 2, b: 2, body: ["  return a + b + 1;"] }], base, cur);
  assertEquals(res.ok, true);
  if (!res.ok) return;
  const out = applyOps(cur, res.ops);
  assertEquals(out[1], "  return a + b + 1;"); // A's edit applied
  assertStringIncludes(out.join("\n"), "// B was here"); // B's edit survived
  assertStringIncludes(out.join("\n"), "return a - b - 1;"); // …both of them
});

Deno.test("PARALLEL: a stale patch over the other write's lines is a conflict", () => {
  const base = toLines(SRC);
  const cur = toLines(SRC.replace("  return a + b;", "  return a + b + 100; // B"));
  const res = rebaseOps(
    [{ kind: "swap", a: 2, b: 2, body: ["  return a + b + 1; // A"] }],
    base,
    cur,
  );
  assertEquals(res.ok, false);
  if (res.ok) return;
  assertStringIncludes(res.conflicts[0].reason, "changed by the other write");
});

Deno.test("PARALLEL: an insert INSIDE the span is a conflict, not a silent clobber", () => {
  // The span's first and last lines still exist, so a naive check would rebase
  // and quietly delete the line the other agent added between them.
  const base = toLines(SRC);
  const cur = toLines(SRC.replace("  return a + b;\n}", "  return a + b;\n  // B added\n}"));
  const res = rebaseOps([{ kind: "swap", a: 1, b: 3, body: ["replacement"] }], base, cur);
  assertEquals(res.ok, false);
  if (res.ok) return;
  assertStringIncludes(res.conflicts[0].reason, "inserted inside");
});
