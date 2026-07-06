import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { digestOutput } from "./digest.ts";

function bigOutput(): string {
  const lines: string[] = ["$ make build", "compiling…"];
  for (let i = 0; i < 2000; i++) lines.push(`[cc] object_${i}.o compiled with flags -O2 -Wall`);
  lines.push("Error: kaboom in src/thing.c:42");
  for (let i = 0; i < 200; i++) lines.push(`[link] linking chunk ${i}`);
  lines.push("make: *** [build] Error 2");
  return lines.join("\n");
}

Deno.test("small output passes through untouched", async () => {
  const completerCalls: string[] = [];
  const out = await digestOutput("just fine", (_s, u) => {
    completerCalls.push(u);
    return Promise.resolve("digest");
  });
  assertEquals(out, "just fine");
  assertEquals(completerCalls.length, 0);
});

Deno.test("big output keeps head and tail and carries the worker digest", async () => {
  const text = bigOutput();
  let seen = "";
  const out = await digestOutput(text, (_s, user) => {
    seen = user;
    return Promise.resolve("build failed: kaboom at src/thing.c:42");
  });
  assertStringIncludes(out, "$ make build"); // head verbatim
  assertStringIncludes(out, "make: *** [build] Error 2"); // tail verbatim
  assertStringIncludes(out, "local-worker digest:");
  assertStringIncludes(out, "kaboom at src/thing.c:42");
  assertStringIncludes(out, "middle lines omitted");
  assertEquals(out.length < text.length / 4, true);
  // The salient middle line was forwarded to the worker.
  assertStringIncludes(seen, "Error: kaboom in src/thing.c:42");
});

Deno.test("worker failure degrades to a plain omission marker, never throws", async () => {
  const out = await digestOutput(bigOutput(), () => Promise.reject(new Error("down")));
  assertStringIncludes(out, "middle lines omitted]");
  assertStringIncludes(out, "$ make build");
  assertEquals(out.includes("local-worker digest"), false);
});

Deno.test("middle without error-looking lines forwards excerpts instead", async () => {
  const lines = ["start"];
  for (let i = 0; i < 3000; i++) lines.push(`plain progress line ${i}`);
  lines.push("end");
  let seen = "";
  await digestOutput(lines.join("\n"), (_s, user) => {
    seen = user;
    return Promise.resolve("lots of progress output, no problems visible");
  });
  assertStringIncludes(seen, "excerpts");
});
