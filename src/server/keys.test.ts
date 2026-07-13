import { assertEquals } from "jsr:@std/assert@1";
import { keyStatus, setEnvVar, setKey } from "./keys.ts";

Deno.test("setEnvVar: appends when the var is absent", () => {
  assertEquals(setEnvVar("FOO=1\n", "OPENAI_API_KEY", "sk-x"), "FOO=1\nOPENAI_API_KEY=sk-x\n");
});

Deno.test("setEnvVar: appends to empty text", () => {
  assertEquals(setEnvVar("", "OPENAI_API_KEY", "sk-x"), "OPENAI_API_KEY=sk-x\n");
});

Deno.test("setEnvVar: replaces an existing line, preserves the rest", () => {
  const before = "# comment\nOPENAI_API_KEY=old\nBAR=2\n";
  assertEquals(
    setEnvVar(before, "OPENAI_API_KEY", "new"),
    "# comment\nOPENAI_API_KEY=new\nBAR=2\n",
  );
});

Deno.test("setEnvVar: replaces a commented-out template line", () => {
  assertEquals(
    setEnvVar("# OPENAI_API_KEY=\nBAR=2\n", "OPENAI_API_KEY", "sk"),
    "OPENAI_API_KEY=sk\nBAR=2\n",
  );
  assertEquals(setEnvVar("#OPENAI_API_KEY=\n", "OPENAI_API_KEY", "sk"), "OPENAI_API_KEY=sk\n");
});

Deno.test("setEnvVar: only the first match is replaced", () => {
  assertEquals(setEnvVar("K=a\nK=b\n", "K", "c"), "K=c\nK=b\n");
});

Deno.test("setEnvVar: idempotent trailing newline (no blank-line accretion)", () => {
  let t = "";
  t = setEnvVar(t, "K", "1");
  t = setEnvVar(t, "K", "2");
  assertEquals(t, "K=2\n");
});

Deno.test("setKey: writes env file 0600, trims, applies to live env", async () => {
  const dir = await Deno.makeTempDir({ prefix: "keys-" });
  const prev = Deno.env.get("OPENAI_API_KEY");
  try {
    const keys = setKey("openai", "  sk-trim  ", dir);
    assertEquals(keys.openai, true);
    assertEquals(Deno.env.get("OPENAI_API_KEY"), "sk-trim"); // trimmed + live
    assertEquals(await Deno.readTextFile(`${dir}/env`), "OPENAI_API_KEY=sk-trim\n");
    assertEquals((await Deno.stat(`${dir}/env`)).mode! & 0o777, 0o600);
  } finally {
    if (prev === undefined) Deno.env.delete("OPENAI_API_KEY");
    else Deno.env.set("OPENAI_API_KEY", prev);
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("keyStatus: booleans only; whitespace is not configured", () => {
  const prev = Deno.env.get("OPENROUTER_API_KEY");
  try {
    Deno.env.set("OPENROUTER_API_KEY", "x");
    assertEquals(keyStatus().openrouter, true);
    Deno.env.set("OPENROUTER_API_KEY", "   ");
    assertEquals(keyStatus().openrouter, false);
  } finally {
    if (prev === undefined) Deno.env.delete("OPENROUTER_API_KEY");
    else Deno.env.set("OPENROUTER_API_KEY", prev);
  }
});
