import { assert, assertEquals, assertThrows } from "jsr:@std/assert@1";
import { githubBundle } from "./bundles.ts";
import { installBundle, InstallError, isInstalled, validateInstall } from "./install.ts";
import { loadConfig } from "./config.ts";

Deno.test("validateInstall: github default contributes the host and passes its fixtures", () => {
  const r = validateInstall(githubBundle);
  assertEquals(r.ok, true);
  assertEquals(r.fixtures.length, 4);
  assert(r.fixtures.every((f) => f.ok));
  assertEquals(r.contribution.allowHosts, ["api.github.com"]);
  assert(r.contribution.rules?.some((rule) => rule.name === "github-graphql-mutation"));
});

Deno.test("validateInstall: wrong param type throws InstallError", () => {
  assertThrows(
    () => validateInstall(githubBundle, { host: 123 as unknown as string }),
    InstallError,
    "must be a string",
  );
});

Deno.test("installBundle: merges into the rule set and marks installed", async () => {
  const dir = await Deno.makeTempDir();
  try {
    const r = installBundle(githubBundle, {}, dir);
    assertEquals(r.ok, true);
    assertEquals(isInstalled("github", dir), true);
    assertEquals(isInstalled("nope", dir), false);

    const cfg = loadConfig(dir);
    assert(cfg.allowHosts.includes("api.github.com"));
    assert(cfg.rules.some((rule) => rule.name === "github-graphql-mutation"));
    assertEquals(cfg.bundles, ["github"]);

    // Idempotent: re-installing doesn't duplicate hosts/rules/bundle names.
    installBundle(githubBundle, {}, dir);
    const again = loadConfig(dir);
    assertEquals(again.allowHosts.filter((h) => h === "api.github.com").length, 1);
    assertEquals(again.rules.filter((rule) => rule.name === "github-graphql-mutation").length, 1);
    assertEquals(again.bundles, ["github"]);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("installBundle: a custom host is contributed to the allowlist", async () => {
  const dir = await Deno.makeTempDir();
  try {
    installBundle(githubBundle, { host: "api.ghe.example" }, dir);
    assert(loadConfig(dir).allowHosts.includes("api.ghe.example"));
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
