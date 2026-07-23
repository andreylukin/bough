import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { GRAFANA_SENTINEL, setupGcx } from "./grafana.ts";

const CONFIG = [
  "contexts:",
  "  default: {}",
  "  grafana-dev:",
  "    grafana:",
  "      server: https://grafana.dev.example.com",
  "      token: dev-token",
  "      org-id: 1",
  "  grafana-dev-alias:",
  "    grafana:",
  "      server: https://grafana.dev.example.com",
  "      token: alias-token",
  "  grafana-prod:",
  "    grafana:",
  "      server: https://grafana.prod.example.com",
  "      token: prod-token",
  "current-context: grafana-dev",
].join("\n");

async function withDirs(fn: (cfg: string, out: string) => void | Promise<void>) {
  const dir = await Deno.makeTempDir();
  const cfg = `${dir}/config.yaml`;
  await Deno.writeTextFile(cfg, CONFIG);
  try {
    await fn(cfg, dir);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
}

Deno.test("setupGcx: sanitized copy carries placeholders; hosts deduped; tokens stamped", async () => {
  await withDirs(async (cfg, out) => {
    const setup = setupGcx(cfg, out)!;
    assertEquals(setup.hosts.sort(), ["grafana.dev.example.com", "grafana.prod.example.com"]);
    const sanitized = await Deno.readTextFile(setup.configPath);
    assertStringIncludes(sanitized, GRAFANA_SENTINEL);
    assertEquals(sanitized.includes("dev-token"), false);
    assertEquals(sanitized.includes("prod-token"), false);
    const dev = setup.credentials.find((c) => c.host === "grafana.dev.example.com")!;
    assertEquals(await (dev.value as () => Promise<string>)(), "Bearer dev-token"); // first context wins
  });
});

Deno.test("setupGcx: token re-read per mint; absent/tokenless config → undefined", async () => {
  await withDirs(async (cfg, out) => {
    const setup = setupGcx(cfg, out)!;
    const prod = setup.credentials.find((c) => c.host === "grafana.prod.example.com")!;
    await Deno.writeTextFile(cfg, CONFIG.replace("prod-token", "rotated"));
    assertEquals(await (prod.value as () => Promise<string>)(), "Bearer rotated");
  });
  assertEquals(setupGcx("/nonexistent/gcx.yaml", "/tmp"), undefined);
});
