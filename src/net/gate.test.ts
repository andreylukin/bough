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

Deno.test("gate: per-session resolver applies different policies by branch", async () => {
  const bus = new Bus();
  const db = new Db(":memory:");
  const open = policy(); // host-open, read-only default
  const deny = policy({ denyHosts: new Set(["api.github.com"]) });
  const gate = createGate({
    db,
    bus,
    resolve: (sessionId) => (sessionId === "locked" ? deny : open),
  });

  assertEquals((await gate.gate(ghGet, { sessionId: "free" })).verdict, "allow");
  assertEquals((await gate.gate(ghGet, { sessionId: "locked" })).verdict, "deny");

  // invalidate() drops the cache so a changed rule set takes effect next request.
  gate.invalidate();
  assertEquals((await gate.gate(ghGet, { sessionId: "free" })).verdict, "allow");
});

Deno.test("gate: expireHolds denies a session's parked holds; others untouched", async () => {
  const h = harness(policy({ mode: "review" }));
  const p1 = h.gate.gate(ghDelete, { sessionId: "s1" });
  const p2 = h.gate.gate(ghDelete, { sessionId: "s2" });
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(h.gate.pending, 2);

  assertEquals(h.gate.expireHolds("s1", "expired — turn ended before approval"), 1);
  const d1 = await p1;
  assertEquals(d1.verdict, "deny");
  assertEquals(d1.reason, "expired — turn ended before approval");
  assertEquals(h.gate.pending, 1);
  // the row flipped to denied with the expiry reason (not "denied by human")
  const row = h.db.recentNetEvents("s1")[0];
  assertEquals(row.verdict, "denied");
  assertEquals(row.reason, "expired — turn ended before approval");

  // undefined session = sweep everything (shutdown)
  assertEquals(h.gate.expireHolds(undefined, "bye"), 1);
  assertEquals((await p2).verdict, "deny");
});

Deno.test("gate: annotator re-emits the parked card with the one-liner", async () => {
  const bus = new Bus();
  const db = new Db(":memory:");
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  let annotate!: (s: string | null) => void;
  const gate = createGate({
    db,
    bus,
    policy: policy({ holdVerbs: new Set(["GET /user"]) }),
    annotator: () => new Promise((r) => (annotate = r)),
  });
  const pending = gate.gate(ghGet, { sessionId: "s1" });
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(events.length, 1); // parked, no annotation yet

  annotate("Reads the authenticated GitHub user profile");
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(events.length, 2); // re-emitted in place, still pending
  const annotated = events[1].data as NetRequest;
  assertEquals(annotated.id, (events[0].data as NetRequest).id);
  assertEquals(annotated.verdict, "pending");
  assertEquals(annotated.annotation, "Reads the authenticated GitHub user profile");

  gate.resolveHold(annotated.id, true);
  await pending;
  // The final verdict keeps the annotation.
  assertEquals((events[2].data as NetRequest).verdict, "allowed");
  assertEquals((events[2].data as NetRequest).annotation, annotated.annotation);
});

Deno.test("gate: a late annotation after resolution is dropped, not re-emitted", async () => {
  const bus = new Bus();
  const db = new Db(":memory:");
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  let annotate!: (s: string | null) => void;
  const gate = createGate({
    db,
    bus,
    policy: policy({ holdVerbs: new Set(["GET /user"]) }),
    annotator: () => new Promise((r) => (annotate = r)),
  });
  const pending = gate.gate(ghGet, { sessionId: "s1" });
  await new Promise((r) => setTimeout(r, 0));
  gate.resolveHold((events[0].data as NetRequest).id, false);
  await pending;
  assertEquals(events.length, 2); // pending, denied

  annotate("too late");
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(events.length, 2); // no pending regression after the final verdict
});

Deno.test("db: expirePendingNetEvents sweeps orphaned pending rows", () => {
  const h = harness();
  h.db.recordNetEvent("sX", {
    id: "stale1",
    host: "api.exa.ai",
    action: "POST /search",
    verdict: "pending",
    ts: 1,
  });
  assertEquals(h.db.expirePendingNetEvents("expired — server restarted"), 1);
  assertEquals(h.db.expirePendingNetEvents("expired — server restarted"), 0);
  const row = h.db.recentNetEvents("sX")[0];
  assertEquals(row.verdict, "denied");
  assertEquals(row.reason, "expired — server restarted");
});
