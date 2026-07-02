/**
 * Manual live smoke test for the turn runner. NOT part of `deno task test` — it
 * makes a real Anthropic call. Requires ANTHROPIC_API_KEY.
 *
 *   deno run --allow-net --allow-env --allow-read --allow-write --allow-run --allow-ffi --allow-sys \
 *     scripts/live-smoke.ts
 *
 * Uses the cheap Haiku model and a trivial prompt, then asserts the supervisor
 * message ended with at least one persisted part and is no longer pending.
 */
import { Db } from "../src/db/db.ts";
import { Bus } from "../src/bus.ts";
import { beginTurn } from "../src/turn.ts";

if (!Deno.env.get("ANTHROPIC_API_KEY")) {
  console.error("ANTHROPIC_API_KEY is not set — skipping live smoke.");
  Deno.exit(2);
}

const db = new Db(":memory:");
const bus = new Bus();
bus.subscribe((e) => {
  if (e.type === "message.delta") Deno.stdout.writeSync(new TextEncoder().encode((e.data as { delta: string }).delta));
});

db.createSession({ id: "s", parentId: null, title: "smoke", kind: "root", createdAt: Date.now() });
db.createMessage({
  id: "u",
  sessionId: "s",
  role: "user",
  parts: [{ type: "text", text: "Reply with exactly the word: pong" }],
  pending: false,
  createdAt: Date.now(),
});

const model = "claude-haiku-4-5-20251001";
const { message, done } = beginTurn({ db, bus, model, workspace: Deno.cwd() }, "s");
await done;

const final = db.getMessage(message.id)!;
console.log("\n---");
console.log("pending:", final.pending);
console.log("parts:", JSON.stringify(final.parts, null, 2));
if (final.pending || final.parts.length === 0) {
  console.error("FAIL: expected a finished message with at least one part");
  Deno.exit(1);
}
console.log("OK");
