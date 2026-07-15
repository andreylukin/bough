import { assertEquals, assertNotEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { Bus } from "../bus.ts";
import { Db } from "../db/db.ts";
import { ClawpatrolGateway } from "./gateway.ts";

/** Run fn with Claw Patrol enabled and the net dir pointed at a temp dir. */
async function withGateway(
  fn: (g: ClawpatrolGateway, bus: Bus) => Promise<void>,
): Promise<void> {
  const dir = await Deno.makeTempDir({ prefix: "bough-gw-" });
  const prevFlag = Deno.env.get("BOUGH_CLAWPATROL");
  const prevDir = Deno.env.get("BOUGH_NET_DIR");
  Deno.env.set("BOUGH_CLAWPATROL", "1");
  Deno.env.set("BOUGH_NET_DIR", dir);
  const bus = new Bus();
  const db = new Db(":memory:");
  const gateway = new ClawpatrolGateway({ db, bus });
  try {
    await gateway.start();
    await fn(gateway, bus);
  } finally {
    await gateway.stop();
    prevFlag ? Deno.env.set("BOUGH_CLAWPATROL", prevFlag) : Deno.env.delete("BOUGH_CLAWPATROL");
    prevDir ? Deno.env.set("BOUGH_NET_DIR", prevDir) : Deno.env.delete("BOUGH_NET_DIR");
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
}

Deno.test("gateway: each session gets its own listener; same session reuses it", async () => {
  await withGateway(async (g) => {
    const [a1, b, a2] = [
      await g.envFor("sA"),
      await g.envFor("sB"),
      await g.envFor("sA"),
    ];
    assertNotEquals(a1.HTTPS_PROXY, b.HTTPS_PROXY);
    assertEquals(a1.HTTPS_PROXY, a2.HTTPS_PROXY);
    assertEquals(g.status().listeners, 2);
    // Trust env rides along, pointed at the shared CA.
    assertEquals(a1.SSL_CERT_FILE, g.status().caPath);
  });
});

Deno.test("gateway: concurrent acquires for one session share one listener", async () => {
  await withGateway(async (g) => {
    const [e1, e2] = await Promise.all([g.envFor("sX"), g.envFor("sX")]);
    assertEquals(e1.HTTPS_PROXY, e2.HTTPS_PROXY);
    assertEquals(g.status().listeners, 1);
  });
});

Deno.test("gateway: turn.finished reaps the session's listener; next turn gets a fresh one", async () => {
  await withGateway(async (g, bus) => {
    const before = await g.envFor("sA");
    await g.envFor("sB");
    assertEquals(g.status().listeners, 2);

    bus.publish({ type: "turn.finished", sessionId: "sA", data: { status: "done" } });
    // release() is fired async off the bus; give it a beat to stop the listener.
    while (g.status().listeners > 1) await new Promise((r) => setTimeout(r, 5));

    assertEquals(g.status().listeners, 1);
    const after = await g.envFor("sA");
    assertNotEquals(after.HTTPS_PROXY, before.HTTPS_PROXY);
  });
});

Deno.test("gateway: envFor injects broker container-creds env when configured", async () => {
  const tokenFile = await Deno.makeTempFile({ prefix: "broker-tok-" });
  await Deno.writeTextFile(tokenFile, "  s3cr3t-boot-token\n");
  Deno.env.set("BOUGH_AWS_BROKER_URL", "http://127.0.0.1:9109/aws");
  Deno.env.set("BOUGH_AWS_BROKER_TOKEN_FILE", tokenFile);
  try {
    await withGateway(async (g) => {
      const env = await g.envFor("sAws");
      assertEquals(env.AWS_CONTAINER_CREDENTIALS_FULL_URI, "http://127.0.0.1:9109/aws");
      assertEquals(env.AWS_CONTAINER_AUTHORIZATION_TOKEN, "s3cr3t-boot-token"); // trimmed
    });

    // Unreadable/absent token file → AWS left unconfigured, rest of env intact.
    await Deno.remove(tokenFile);
    await withGateway(async (g) => {
      const env = await g.envFor("sAws2");
      assertEquals(env.AWS_CONTAINER_CREDENTIALS_FULL_URI, undefined);
      assertEquals(env.AWS_CONTAINER_AUTHORIZATION_TOKEN, undefined);
      assertNotEquals(env.HTTPS_PROXY, undefined);
    });
  } finally {
    Deno.env.delete("BOUGH_AWS_BROKER_URL");
    Deno.env.delete("BOUGH_AWS_BROKER_TOKEN_FILE");
  }
});

Deno.test("gateway: BOUGH_CLAWPATROL=0 opts out — no listeners, empty env", async () => {
  const prev = Deno.env.get("BOUGH_CLAWPATROL");
  Deno.env.set("BOUGH_CLAWPATROL", "0");
  try {
    const gateway = new ClawpatrolGateway({ db: new Db(":memory:"), bus: new Bus() });
    await gateway.start();
    assertEquals(await gateway.envFor("s1"), {});
    assertEquals(gateway.status().running, false);
    await gateway.stop();
  } finally {
    prev !== undefined
      ? Deno.env.set("BOUGH_CLAWPATROL", prev)
      : Deno.env.delete("BOUGH_CLAWPATROL");
  }
});

Deno.test("gateway: turn.finished expires the session's parked holds", async () => {
  await withGateway(async (g, bus) => {
    await g.envFor("sH"); // gate is up
    const parked = g.gate!.gate(
      { host: "nowhere.example.com", method: "POST", path: "/x" },
      { sessionId: "sH" },
    );
    await new Promise((r) => setTimeout(r, 0));
    assertEquals(g.gate!.pending, 1);

    bus.publish({ type: "turn.finished", sessionId: "sH", data: { status: "interrupted" } });
    const decision = await parked;
    assertEquals(decision.verdict, "deny");
    assertEquals(g.gate!.pending, 0);
  });
});

Deno.test("gateway: envFor points kubectl at the rewritten config + a per-session cache dir", async () => {
  // A minimal EKS-shaped kubeconfig (mirrors kubeconfig.test.ts): one cluster,
  // exec auth — enough for setupKube to rewrite it and arm the kube env.
  const kc = await Deno.makeTempFile({ prefix: "bough-gw-kc-", suffix: ".yaml" });
  const ca = "-----BEGIN CERTIFICATE-----\nCLUSTERROOT\n-----END CERTIFICATE-----\n";
  await Deno.writeTextFile(
    kc,
    [
      "apiVersion: v1",
      "kind: Config",
      "clusters:",
      "- name: dev",
      "  cluster:",
      "    server: https://EABF96.gr7.us-east-2.eks.amazonaws.com",
      `    certificate-authority-data: ${btoa(ca)}`,
      "users:",
      "- name: dev",
      "  user:",
      "    exec:",
      "      apiVersion: client.authentication.k8s.io/v1beta1",
      "      command: aws",
      "      args: [eks, get-token, --cluster-name, dev]",
      "contexts:",
      "- name: dev",
      "  context: { cluster: dev, user: dev }",
      "current-context: dev",
      "",
    ].join("\n"),
  );
  const prev = Deno.env.get("KUBECONFIG");
  Deno.env.set("KUBECONFIG", kc);
  try {
    await withGateway(async (g) => {
      const env = await g.envFor("sKube");
      // KUBECONFIG is the CA-rewritten copy in the net dir, never the original.
      assertNotEquals(env.KUBECONFIG, kc);
      assertStringIncludes(env.KUBECONFIG, "kubeconfig");
      // The cache dir is per-session, exists, and sits under the temp write-allow.
      assertStringIncludes(env.KUBECACHEDIR, "bough-kube-cache");
      assertStringIncludes(env.KUBECACHEDIR, "sKube");
      const st = await Deno.stat(env.KUBECACHEDIR);
      assertEquals(st.isDirectory, true);
    });
  } finally {
    prev !== undefined ? Deno.env.set("KUBECONFIG", prev) : Deno.env.delete("KUBECONFIG");
    await Deno.remove(kc).catch(() => {});
  }
});
