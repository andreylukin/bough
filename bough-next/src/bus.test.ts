import { assertEquals } from "jsr:@std/assert@1";
import { Bus } from "./bus.ts";
import type { BoughEvent } from "./schema/parts.ts";

Deno.test("publish stamps monotonic seq + ts and returns the event", () => {
  const bus = new Bus();
  const a = bus.publish({ type: "x", data: 1 });
  const b = bus.publish({ type: "y", data: 2 });
  assertEquals(a.seq, 1);
  assertEquals(b.seq, 2);
  assertEquals(typeof a.ts, "number");
});

Deno.test("subscribe receives events; unsubscribe stops them", () => {
  const bus = new Bus();
  const got: BoughEvent[] = [];
  const off = bus.subscribe((e) => got.push(e));
  bus.publish({ type: "a", data: null });
  off();
  bus.publish({ type: "b", data: null });
  assertEquals(got.map((e) => e.type), ["a"]);
  assertEquals(bus.size, 0);
});

Deno.test("a throwing listener does not break fan-out to others", () => {
  const bus = new Bus();
  const got: string[] = [];
  bus.subscribe(() => {
    throw new Error("boom");
  });
  bus.subscribe((e) => got.push(e.type));
  bus.publish({ type: "ok", data: null });
  assertEquals(got, ["ok"]);
});
