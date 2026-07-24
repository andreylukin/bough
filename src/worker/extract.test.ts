import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { extractFrom } from "./extract.ts";

Deno.test("free-form extraction returns the trimmed worker reply", async () => {
  let seen = "";
  const out = await extractFrom(
    "deno 2.1.4 is installed",
    "the deno version",
    undefined,
    (_s, u) => {
      seen = u;
      return Promise.resolve("  2.1.4\n");
    },
  );
  assertEquals(out, "2.1.4");
  assertStringIncludes(seen, "deno 2.1.4 is installed");
  assertStringIncludes(seen, "EXTRACT: the deno version");
});

Deno.test("a schema is forwarded to the worker and the reply comes back parsed", async () => {
  let seenSchema: unknown;
  const schema = { type: "object", properties: { version: { type: "string" } } };
  const out = await extractFrom("deno 2.1.4", "the version", schema, (_s, _u, js) => {
    seenSchema = js;
    return Promise.resolve(`{"version":"2.1.4"}`);
  });
  assertEquals(out, { version: "2.1.4" });
  assertEquals(seenSchema, schema);
});

Deno.test("a non-JSON reply under a schema throws rather than degrading", async () => {
  const err = await assertRejects(() =>
    extractFrom("text", "thing", { type: "object" }, () => Promise.resolve("sure! here you go"))
  );
  assertStringIncludes((err as Error).message, "did not parse as JSON");
});

Deno.test("oversized text is refused with a slice-it instruction", async () => {
  const err = await assertRejects(() =>
    extractFrom("x".repeat(12_001), "thing", undefined, () => Promise.resolve("nope"))
  );
  assertStringIncludes((err as Error).message, "over the 12000-char worker limit");
});
