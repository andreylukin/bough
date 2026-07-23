import { assertEquals } from "jsr:@std/assert@1";
import { setupArgocd } from "./argocd.ts";

const CONFIG = [
  "contexts:",
  "- name: argocd.dev.example.com",
  "  server: argocd.dev.example.com",
  "  user: argocd.dev.example.com",
  "- name: argocd.prod.example.com",
  "  server: argocd.prod.example.com",
  "  user: argocd.prod.example.com",
  "current-context: argocd.prod.example.com",
  "servers:",
  "- grpc-web: true",
  "  server: argocd.dev.example.com",
  "users:",
  "- name: argocd.dev.example.com",
  "  auth-token: dev-token",
  "- name: argocd.prod.example.com",
  "  auth-token: prod-token",
].join("\n");

async function withConfig(text: string, fn: (path: string) => void | Promise<void>) {
  const dir = await Deno.makeTempDir();
  const path = `${dir}/config`;
  await Deno.writeTextFile(path, text);
  try {
    await fn(path);
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
}

Deno.test("setupArgocd: current-context wins; every tokened server gets a rule", async () => {
  await withConfig(CONFIG, async (path) => {
    const setup = setupArgocd(path)!;
    assertEquals(setup.server, "argocd.prod.example.com");
    assertEquals(setup.hosts.sort(), ["argocd.dev.example.com", "argocd.prod.example.com"]);
    const dev = setup.credentials.find((c) => c.host === "argocd.dev.example.com")!;
    assertEquals(await (dev.value as () => Promise<string>)(), "Bearer dev-token");
  });
});

Deno.test("setupArgocd: token re-read per mint — host re-login takes effect live", async () => {
  await withConfig(CONFIG, async (path) => {
    const setup = setupArgocd(path)!;
    const prod = setup.credentials.find((c) => c.host === "argocd.prod.example.com")!;
    assertEquals(await (prod.value as () => Promise<string>)(), "Bearer prod-token");
    await Deno.writeTextFile(path, CONFIG.replace("prod-token", "renewed-token"));
    assertEquals(await (prod.value as () => Promise<string>)(), "Bearer renewed-token");
  });
});

Deno.test("setupArgocd: absent / tokenless config → undefined", async () => {
  assertEquals(setupArgocd("/nonexistent/argocd/config"), undefined);
  await withConfig("contexts:\n- name: a\n  server: a\n  user: a\nusers:\n- name: a\n", (path) => {
    assertEquals(setupArgocd(path), undefined);
  });
});
