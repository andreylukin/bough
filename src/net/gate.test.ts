import { assertEquals } from "jsr:@std/assert@1";
import { Bus } from "../bus.ts";
import { Db } from "../db/db.ts";
import { createGate } from "./gate.ts";
import { policy } from "./policy.ts";
import type { BoughEvent, NetRequest } from "../schema/parts.ts";

function harness(pol = policy()) {
  const bus = new Bus();
  const db = new Db(":memory:");
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  return { gate: createGate({ db, bus, policy: pol }), db, events };
}

const ghGet = { host: "api.github.com", method: "GET", path: "/user" };
const ghDelete = { host: "api.github.com", method: "DELETE", path: "/repos/o/r" };

Deno.test("gate: read is allowed, persisted, and emitted as net.request", async () => {
  const h = harness();
  const d = await h.gate.gate(ghGet, { sessionId: "s1", requestedBy: "worker" });
  assertEquals(d.verdict, "allow");

  assertEquals(h.events.length, 1);
  assertEquals(h.events[0].type, "net.request");
  const nr = h.events[0].data as NetRequest;
  assertEquals(nr.verdict, "allowed");
  assertEquals(nr.host, "api.github.com");
  assertEquals(nr.requestedBy, "worker");

  const recent = h.db.recentNetEvents("s1");
  assertEquals(recent.map((r) => r.verdict), ["allowed"]);
});

Deno.test("gate: write is denied under read_only default", async () => {
  const h = harness();
  const d = await h.gate.gate(ghDelete, { sessionId: "s1" });
  assertEquals(d.verdict, "deny");
  assertEquals((h.events[0].data as NetRequest).verdict, "denied");
});

Deno.test("gate: hold parks until approved, then flips to allowed", async () => {
  const h = harness(policy({ holdVerbs: new Set(["GET /user"]) }));
  const pending = h.gate.gate(ghGet, { sessionId: "s1" });

  // Emitted immediately as pending; the caller is parked.
  assertEquals(h.gate.pending, 1);
  const first = h.events[0].data as NetRequest;
  assertEquals(first.verdict, "pending");

  assertEquals(h.gate.resolveHold(first.id, true), true);
  const d = await pending;
  assertEquals(d.verdict, "allow");

  // Re-emitted with the final verdict on the same id (approval card updates in place).
  assertEquals((h.events[1].data as NetRequest).id, first.id);
  assertEquals((h.events[1].data as NetRequest).verdict, "allowed");
  assertEquals(h.gate.pending, 0);
  assertEquals(h.db.recentNetEvents("s1").length, 1); // upsert, not a second row
});

Deno.test("gate: hold denied flips to denied; unknown id is a no-op", async () => {
  const h = harness(policy({ holdVerbs: new Set(["GET /user"]) }));
  const pending = h.gate.gate(ghGet, { sessionId: "s1" });
  const id = (h.events[0].data as NetRequest).id;

  assertEquals(h.gate.resolveHold("does-not-exist", true), false);
  assertEquals(h.gate.resolveHold(id, false), true);
  assertEquals((await pending).verdict, "deny");
  assertEquals((h.events[1].data as NetRequest).verdict, "denied");
});

Deno.test("gate: setPolicy swaps enforcement for the next request", async () => {
  const h = harness(); // read_only default → write denied
  assertEquals((await h.gate.gate(ghDelete)).verdict, "deny");
  h.gate.setPolicy(policy({ mode: "all" }));
  assertEquals((await h.gate.gate(ghDelete)).verdict, "allow");
});
