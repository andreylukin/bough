import { assertEquals } from "jsr:@std/assert@1";
import { checkSyntax, unterminatedString } from "./text.ts";
import { PROGRAM_PARAMS } from "./harness/vm.ts";

Deno.test("unterminatedString: finds a newline-closed quote, ignores legal newlines", () => {
  // The field failure: a template literal ate the \n meant for this string.
  const hit = unterminatedString(`const a = 1;\nconst p = "hello\nworld";\n`)!;
  assertEquals(hit.line, 2);
  assertEquals(hit.quote, '"');
  assertEquals(hit.text.includes('"hello'), true);
  // Template literals, comments and escaped quotes all span/contain newlines legally.
  assertEquals(unterminatedString("const t = `line one\nline two`;\n"), null);
  assertEquals(unterminatedString("// a comment\nconst s = 'ok';\n"), null);
  assertEquals(unterminatedString('/* multi\nline */\nconst s = "ok";\n'), null);
  assertEquals(unterminatedString(`const s = "she said \\"hi\\"";\n`), null);
  assertEquals(unterminatedString("const t = `a ${ {x: 'y'} } b`;\n"), null);
});

Deno.test("checkSyntax: clean code passes; a broken string names its line and the fix", () => {
  assertEquals(checkSyntax('const p = "one\\ntwo";\nawait bash(p)', ["bash"], "program"), null);
  const bad = checkSyntax('const p = "one\ntwo";', ["bash"], "program")!;
  assertEquals(bad.message.includes("line 1"), true);
  assertEquals(bad.message.includes("consumed by the outer literal"), true);
  // A shadowed host name is a SyntaxError too — no position to guess, but the
  // reason still gets through instead of a ten-frame Deno stack.
  const shadow = checkSyntax("let bash = 1;", ["bash"], "program")!;
  assertEquals(shadow.message.includes("already been declared"), true);
});

Deno.test("PROGRAM_PARAMS matches vm_worker's own AsyncFunction arity", async () => {
  // The worker imports nothing (permissions: "none"), so the list is duplicated.
  // Drift would make the pre-flight check disagree with the real compile.
  const src = await Deno.readTextFile(new URL("./harness/vm_worker.ts", import.meta.url));
  const call = src.slice(src.indexOf("const program = new AsyncFunction("));
  const names = [...call.slice(0, call.indexOf("code,")).matchAll(/"([a-zA-Z]+)"/g)].map((m) =>
    m[1]
  );
  assertEquals(names, PROGRAM_PARAMS);
});
