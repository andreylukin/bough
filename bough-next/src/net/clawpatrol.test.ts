import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { clawpatrolAvailable, clawpatrolTest } from "./clawpatrol.ts";
import { githubBundle } from "./bundles.ts";
import { validateInstall } from "./install.ts";

// Integration against the REAL clawpatrol binary — skipped where it isn't installed
// so CI without the tool stays green. Locally this is the authoritative regression.
const available = clawpatrolAvailable();

Deno.test({
  name: "clawpatrol accepts the rendered github policy and all bundle fixtures",
  ignore: !available,
  async fn() {
    const r = validateInstall(githubBundle, {});
    const real = await clawpatrolTest(r.hcl, githubBundle.fixtures);
    assertEquals(real.ran, true);
    assert(real.ok, `clawpatrol drift:\n${real.output}`);
  },
});

Deno.test({
  name: "clawpatrol catches verdict drift (a wrong fixture fails the run)",
  ignore: !available,
  async fn() {
    const r = validateInstall(githubBundle, {});
    const lying = [{
      name: "gh-delete-should-be-allowed",
      action: {
        host: "api.github.com",
        http: { method: "DELETE", path: "/repos/o/r", headers: {} },
      },
      expect: { verdict: "allow" as const },
    }];
    const real = await clawpatrolTest(r.hcl, lying);
    assertEquals(real.ran, true);
    assertEquals(real.ok, false);
    assertStringIncludes(real.output, "mismatch");
  },
});
