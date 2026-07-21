import { assertEquals } from "jsr:@std/assert@1";
import { renderConvo, sanitizeSuggestion } from "./suggest.ts";

Deno.test("renderConvo: roles label lines, oldest first", () => {
  assertEquals(
    renderConvo([
      { role: "user", text: "fix the flaky test" },
      { role: "agent", text: "Done — turn.test.ts passes now." },
    ]),
    "user: fix the flaky test\nagent: Done — turn.test.ts passes now.",
  );
});

Deno.test("renderConvo: long lines keep their tail, capped to the last 8 lines", () => {
  const long = "x".repeat(700) + "THE END";
  const out = renderConvo([{ role: "agent", text: long }]);
  assertEquals(out.startsWith("agent: …"), true);
  assertEquals(out.endsWith("THE END"), true);
  const many = Array.from({ length: 12 }, (_, i) => ({
    role: "user" as const,
    text: `m${i}`,
  }));
  assertEquals(renderConvo(many).split("\n").length, 8);
  assertEquals(renderConvo(many).split("\n")[0], "user: m4");
});

Deno.test("sanitizeSuggestion: labels and quotes strip, first real line wins", () => {
  assertEquals(sanitizeSuggestion('user: "run the tests"'), "run the tests");
  assertEquals(sanitizeSuggestion("\n\ncommit it\nand more prose"), "commit it");
});

Deno.test("sanitizeSuggestion: empty/whitespace replies are rejected", () => {
  assertEquals(sanitizeSuggestion("   \n  "), null);
  assertEquals(sanitizeSuggestion('""'), null);
});

Deno.test("sanitizeSuggestion: overlong replies are capped", () => {
  assertEquals(sanitizeSuggestion("y".repeat(400))!.length, 150);
});
