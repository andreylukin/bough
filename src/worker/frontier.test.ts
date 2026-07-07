import { assertEquals } from "jsr:@std/assert@1";
import { frontierWorkerModel } from "./frontier.ts";

function withEnv(vars: Record<string, string>, fn: () => void) {
  for (const [k, v] of Object.entries(vars)) Deno.env.set(k, v);
  try {
    fn();
  } finally {
    for (const k of Object.keys(vars)) Deno.env.delete(k);
  }
}

Deno.test("frontierWorkerModel: off by default", () => {
  assertEquals(frontierWorkerModel(), null);
});

Deno.test("frontierWorkerModel: '1' means the default model, other values are model ids", () => {
  withEnv({ BOUGH_WORKER_FRONTIER: "1" }, () => {
    assertEquals(frontierWorkerModel(), "claude-haiku-4-5");
  });
  withEnv({ BOUGH_WORKER_FRONTIER: "claude-sonnet-5" }, () => {
    assertEquals(frontierWorkerModel(), "claude-sonnet-5");
  });
  withEnv({ BOUGH_WORKER_FRONTIER: "0" }, () => {
    assertEquals(frontierWorkerModel(), null);
  });
});

Deno.test("frontierWorkerModel: BOUGH_WORKER_LOCAL_ONLY wins — privacy tier is never overridden", () => {
  withEnv({ BOUGH_WORKER_FRONTIER: "1", BOUGH_WORKER_LOCAL_ONLY: "1" }, () => {
    assertEquals(frontierWorkerModel(), null);
  });
});
