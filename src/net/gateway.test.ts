import { assertEquals, assertNotEquals } from "jsr:@std/assert@1";
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
    gateway.start();
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

Deno.test("gateway: disabled flag means no listeners and empty env", async () => {
  const prev = Deno.env.get("BOUGH_CLAWPATROL");
  Deno.env.delete("BOUGH_CLAWPATROL");
  try {
    const gateway = new ClawpatrolGateway({ db: new Db(":memory:"), bus: new Bus() });
    gateway.start();
    assertEquals(await gateway.envFor("s1"), {});
    assertEquals(gateway.status().running, false);
    await gateway.stop();
  } finally {
    if (prev) Deno.env.set("BOUGH_CLAWPATROL", prev);
  }
});
