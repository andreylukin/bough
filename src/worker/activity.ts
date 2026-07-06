/**
 * Live-map activity blurbs — a bus listener (wired in the server entry only, so
 * turn tests stay hermetic) that watches each session's run_steps programs go by
 * and publishes a present-tense one-liner ("running the test suite") as ephemeral
 * `session.activity` events. Nothing persists; the UI keeps the latest per session.
 *
 * One in-flight blurb per session: rounds that land while the worker is busy are
 * dropped, not queued — the next round will describe itself.
 */
import type { Bus } from "../bus.ts";
import type { Part } from "../schema/parts.ts";
import { activityBlurb } from "./annotate.ts";

/** Start watching. Returns the unsubscribe. */
export function watchActivity(
  bus: Bus,
  blurb: (code: string, outputHead: string) => Promise<string | null> = activityBlurb,
): () => void {
  const inflight = new Set<string>();
  return bus.subscribe((e) => {
    if (e.type !== "message.part" || !e.sessionId) return;
    const part = (e.data as { part?: Part }).part;
    if (part?.type !== "tool_call" || part.name !== "run_steps") return;
    const code = (part.input as { code?: string })?.code;
    if (!code) return;
    const sessionId = e.sessionId;
    if (inflight.has(sessionId)) return;
    inflight.add(sessionId);
    blurb(code, "")
      .then((text) => {
        if (text) {
          bus.publish({ type: "session.activity", sessionId, data: { text, ts: Date.now() } });
        }
      })
      .catch(() => {})
      .finally(() => inflight.delete(sessionId));
  });
}
