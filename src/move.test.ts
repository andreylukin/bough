import { assertEquals, assertThrows } from "jsr:@std/assert@1";
import { Db } from "./db/db.ts";
import { Bus } from "./bus.ts";
import { move, MoveError } from "./move.ts";
import type { Message, Session } from "./schema/parts.ts";

function ses(id: string): Session {
  return { id, parentId: null, title: id, kind: "root", createdAt: 1 };
}
function msg(id: string, sessionId: string, createdAt: number, text: string): Message {
  return {
    id,
    sessionId,
    role: "user",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt,
  };
}

Deno.test("move: appends copies of picked source messages onto the target", () => {
  const db = new Db(":memory:");
  const ctx = { db, bus: new Bus() };
  db.createSession(ses("src"));
  db.createSession(ses("dst"));
  db.createMessage(msg("s1", "src", 1, "alpha"));
  db.createMessage(msg("s2", "src", 2, "beta"));
  db.createMessage(msg("s3", "src", 3, "gamma"));
  db.createMessage(msg("d1", "dst", 1, "existing"));

  const target = move(ctx, "dst", {
    sourceId: "src",
    picks: [{ messageId: "s2" }, { messageId: "s3" }],
  });
  assertEquals(target.id, "dst");
  const texts = db.threadFor("dst").map((m) => (m.parts[0] as { text: string }).text);
  assertEquals(texts, ["existing", "beta", "gamma"]); // appended in order, fresh copies
  // source is untouched (non-destructive copy)
  assertEquals(db.threadFor("src").length, 3);
  db.close();
});

Deno.test("move: rejects same-session, unknown target, and out-of-thread picks", () => {
  const db = new Db(":memory:");
  const ctx = { db, bus: new Bus() };
  db.createSession(ses("a"));
  db.createMessage(msg("m1", "a", 1, "x"));
  assertThrows(() => move(ctx, "a", { sourceId: "a", picks: [{ messageId: "m1" }] }), MoveError);
  assertThrows(() => move(ctx, "gone", { sourceId: "a", picks: [{ messageId: "m1" }] }), MoveError);
  db.createSession(ses("b"));
  assertThrows(
    () => move(ctx, "b", { sourceId: "a", picks: [{ messageId: "nope" }] }),
    MoveError,
  );
  db.close();
});
