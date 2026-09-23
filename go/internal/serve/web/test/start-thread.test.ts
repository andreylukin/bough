import { afterEach, expect, test } from "bun:test";
import { api } from "../src/api";

const origFetch = globalThis.fetch;
afterEach(() => { globalThis.fetch = origFetch; });

// A bare spawnedBy is an agent's background child, which serve holds at
// depth 1: a thread the person starts from main must go as mode project.
test("Start thread posts a project thread, not a background child of main", async () => {
  let sent: Record<string, unknown> = {};
  globalThis.fetch = (async (_: string, init?: RequestInit) => {
    sent = JSON.parse(String(init?.body));
    return new Response(JSON.stringify({ session: { id: "t1" }, queued: false }), { status: 201 });
  }) as typeof fetch;
  const row = await api.createThread("look into it", "vdemo");
  expect(row.id).toBe("t1");
  expect(sent).toEqual({ prompt: "look into it", mode: "project", project: "vdemo" });
});
