import { assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import type { BoughEvent, Session } from "../schema/parts.ts";
import { maybeAutoTitle, type Titler, UNTITLED } from "./title.ts";

function harness(title: string) {
  const db = new Db(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const s: Session = { id: "s1", parentId: null, title, kind: "root", createdAt: 1 };
  db.createSession(s);
  return { db, bus, events };
}

/** One-shot fake titler that records its calls. */
function fakeTitler(reply: string): Titler & { calls: string[] } {
  const calls: string[] = [];
  const fn = ((text: string) => {
    calls.push(text);
    return Promise.resolve(reply);
  }) as Titler & { calls: string[] };
  fn.calls = calls;
  return fn;
}

Deno.test("untitled session gets a worker-generated title + session.updated", async () => {
  const { db, bus, events } = harness(UNTITLED);
  const titler = fakeTitler('  "Fix login redirect"  ');
  maybeAutoTitle({ db, bus, titler }, "s1", "the login page redirects to /404 after auth");
  await new Promise((r) => setTimeout(r, 0));

  assertEquals(db.getSession("s1")?.title, "Fix login redirect");
  const updated = events.find((e) => e.type === "session.updated");
  assertEquals((updated?.data as Session).title, "Fix login redirect");
});

Deno.test("small-model decoration is stripped (label, quotes, extra lines)", async () => {
  const { db, bus } = harness(UNTITLED);
  const titler = fakeTitler(
    'Title: **"Debug flaky tests"**\nHere is a short title for the session.',
  );
  maybeAutoTitle({ db, bus, titler }, "s1", "tests flake on CI");
  await new Promise((r) => setTimeout(r, 0));

  assertEquals(db.getSession("s1")?.title, "Debug flaky tests");
});

Deno.test("titled session is left alone", async () => {
  const { db, bus, events } = harness("main");
  const titler = fakeTitler("never used");
  maybeAutoTitle({ db, bus, titler }, "s1", "hello");
  await new Promise((r) => setTimeout(r, 0));

  assertEquals(titler.calls.length, 0);
  assertEquals(db.getSession("s1")?.title, "main");
  assertEquals(events.some((e) => e.type === "session.updated"), false);
});
