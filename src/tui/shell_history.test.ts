import { assertEquals } from "jsr:@std/assert";
import { parseShellHistory } from "./shell_history.ts";

Deno.test("parseShellHistory: plain bash-style lines pass through", () => {
  assertEquals(parseShellHistory("git status\nls -la\n"), ["git status", "ls -la"]);
});

Deno.test("parseShellHistory: zsh extended-history metadata is stripped", () => {
  assertEquals(
    parseShellHistory(": 1700000000:0;git log --oneline\n: 1700000001:2;deno task test\n"),
    ["git log --oneline", "deno task test"],
  );
});

Deno.test("parseShellHistory: blank lines drop, trailing backslash trims", () => {
  assertEquals(parseShellHistory("\n\necho hi \\\n"), ["echo hi"]);
});
