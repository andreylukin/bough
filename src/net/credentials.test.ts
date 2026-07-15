import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { bindingRules, resolveCredentials } from "./credentials.ts";
import { NetConfig } from "./config.ts";
import type { CredentialRule } from "./proxy.ts";

function cfg(credentials: NetConfig["credentials"]): NetConfig {
  return NetConfig.parse({ credentials });
}

async function valueOf(rule: CredentialRule): Promise<string> {
  return typeof rule.value === "string" ? rule.value : await rule.value();
}

Deno.test("bindingRules: reads the token from bough's env at call time (rotation-safe)", async () => {
  Deno.env.set("BOUGH_TEST_GH_TOKEN", "ghp_first");
  try {
    const [rule] = bindingRules([{
      host: "api.github.com",
      header: "authorization",
      env: "BOUGH_TEST_GH_TOKEN",
    }]);
    assertEquals(rule.host, "api.github.com");
    assertEquals(rule.header, "authorization");
    assertEquals(await valueOf(rule), "Bearer ghp_first");
    // rotate the env var — the next stamp picks it up without rebuilding the rule
    Deno.env.set("BOUGH_TEST_GH_TOKEN", "ghp_second");
    assertEquals(await valueOf(rule), "Bearer ghp_second");
  } finally {
    Deno.env.delete("BOUGH_TEST_GH_TOKEN");
  }
});

Deno.test("bindingRules: a custom template places the token verbatim", async () => {
  Deno.env.set("BOUGH_TEST_KEY", "sk-123");
  try {
    const [rule] = bindingRules([
      { host: "api.example.com", header: "x-api-key", env: "BOUGH_TEST_KEY", template: "{token}" },
    ]);
    assertEquals(await valueOf(rule), "sk-123");
  } finally {
    Deno.env.delete("BOUGH_TEST_KEY");
  }
});

Deno.test("bindingRules: an unset env var rejects with a clear message (→ 502 at the proxy)", async () => {
  Deno.env.delete("BOUGH_TEST_MISSING");
  const [rule] = bindingRules([{ host: "h", header: "authorization", env: "BOUGH_TEST_MISSING" }]);
  const err = await assertRejects(() => valueOf(rule), Error);
  assertStringIncludes(err.message, "BOUGH_TEST_MISSING is unset");
});

Deno.test("resolveCredentials: bundle bindings come first, kube exec creds last", async () => {
  Deno.env.set("BOUGH_TEST_TOK", "t");
  try {
    const kube: CredentialRule[] = [{
      host: "eks.example.com",
      header: "authorization",
      value: () => Promise.resolve("Bearer minted"),
    }];
    const rules = resolveCredentials(
      cfg([{ host: "api.github.com", header: "authorization", env: "BOUGH_TEST_TOK" }]),
      kube,
    );
    assertEquals(rules.map((r) => r.host), ["api.github.com", "eks.example.com"]);
    assertEquals(await valueOf(rules[0]), "Bearer t");
    assertEquals(await valueOf(rules[1]), "Bearer minted");
  } finally {
    Deno.env.delete("BOUGH_TEST_TOK");
  }
});

Deno.test("resolveCredentials: no bindings and no kube → empty", () => {
  assertEquals(resolveCredentials(cfg([])).length, 0);
});
