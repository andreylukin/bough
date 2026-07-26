import { assertEquals } from "jsr:@std/assert@1";
import { chunkInput, stripCtl } from "./App.tsx";

// Regression: fast typing followed by ^j coalesces into one "line\n" chunk. A
// bare \n must stay a literal newline — treating it as "send" shipped a
// half-written message (and paid for the turn).
Deno.test("chunkInput: a coalesced ^j newline never sends", () => {
  assertEquals(chunkInput("line one\n"), { body: "line one\n", send: false });
  assertEquals(chunkInput("\n"), { body: "\n", send: false });
});

Deno.test("chunkInput: a trailing \\r still sends (Return / pasted block)", () => {
  assertEquals(chunkInput("line one\r"), { body: "line one", send: true });
  assertEquals(chunkInput("a\rb\r"), { body: "a\nb", send: true });
});

Deno.test("chunkInput: strips invisible control bytes", () => {
  assertEquals(chunkInput("\u0015!ls\r"), { body: "!ls", send: true });
});

Deno.test("stripCtl: keeps tabs and newlines", () => {
  assertEquals(stripCtl("a\tb\nc\u0000\u001b\u007f"), "a\tb\nc");
});
