import { assert, assertEquals, assertThrows } from "jsr:@std/assert@1";
import { githubBundle } from "./bundles.ts";
import { installBundle, InstallError, isInstalled, validateInstall } from "./install.ts";
import { loadConfig } from "./config.ts";

Deno.test("validateInstall: github default contributes API + git hosts and passes its fixtures", () => {
  const r = validateInstall(githubBundle);
  assertEquals(r.ok, true);
  assertEquals(r.fixtures.length, 6); // + git-fetch, git-push
  assert(r.fixtures.every((f) => f.ok));
  assertEquals(r.contribution.allowHosts, ["api.github.com", "github.com"]);
  assert(r.contribution.rules?.some((rule) => rule.name === "github-graphql-mutation"));
  // git fetch allowed, push held — path-distinguishable
  assert(r.contribution.rules?.some((rule) => rule.name === "github-git-fetch" && rule.verdict === "allow"));
  assert(r.contribution.rules?.some((rule) => rule.name === "github-git-push" && rule.verdict === "hold"));
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
    const hosts = loadConfig(dir).allowHosts;
    assert(hosts.includes("api.ghe.example"));
    assert(hosts.includes("ghe.example")); // derived git host (api. prefix stripped)
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("installBundle: no tokenEnv → no credential binding (injection off by default)", () => {
  const r = validateInstall(githubBundle);
  assertEquals(r.contribution.credentials, undefined);
});

Deno.test("installBundle: tokenEnv contributes a credential binding, persisted with only the var name", async () => {
  const dir = await Deno.makeTempDir();
  try {
    const r = validateInstall(githubBundle, { tokenEnv: "MY_GH_TOKEN" });
    // Injected on BOTH the API host (gh) and the git host (git clone/push over HTTPS).
    assertEquals(r.contribution.credentials, [
      { host: "api.github.com", header: "authorization", env: "MY_GH_TOKEN" },
      { host: "github.com", header: "authorization", env: "MY_GH_TOKEN" },
    ]);
    installBundle(githubBundle, { tokenEnv: "MY_GH_TOKEN" }, dir);
    const cfg = loadConfig(dir);
    assertEquals(cfg.credentials, [
      { host: "api.github.com", header: "authorization", env: "MY_GH_TOKEN" },
      { host: "github.com", header: "authorization", env: "MY_GH_TOKEN" },
    ]);
    // re-install with a changed env var replaces the bindings in place (dedup by host+header)
    installBundle(githubBundle, { tokenEnv: "OTHER_TOKEN" }, dir);
    const again = loadConfig(dir);
    assertEquals(again.credentials, [
      { host: "api.github.com", header: "authorization", env: "OTHER_TOKEN" },
      { host: "github.com", header: "authorization", env: "OTHER_TOKEN" },
    ]);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});
