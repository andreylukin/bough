/**
 * The bench server: start it, wait for it, stop it.
 *
 * WHY IT RESTARTS PER SWEEP BY DEFAULT. A server left over from a previous sweep is
 * running the PREVIOUS code and the PREVIOUS prompt files — the sections are read
 * and memoized at first use (`prompt/assemble.ts`), so a stale process silently
 * serves the pre-edit prompt while the results file records the post-edit sha. That
 * invalidates the one measurement this whole apparatus exists to make, and it does
 * it without any visible symptom. `AHE_KEEP_SERVER=1` opts out; nothing in the loop
 * sets it.
 */
import { spawn } from "node:child_process";
import { mkdirSync } from "node:fs";
import { HOME, PORT, REPO, TRACE_DIR } from "./config.ts";

const base = `http://127.0.0.1:${PORT}`;

/** Is a server answering on the bench port? */
export async function isUp(): Promise<boolean> {
  try {
    const res = await fetch(`${base}/sessions`, { signal: AbortSignal.timeout(1500) });
    return res.ok;
  } catch {
    return false;
  }
}

async function waitUntil(want: boolean, ms = 30_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (await isUp() === want) return;
    await Bun.sleep(250);
  }
  throw new Error(`bench server did not come ${want ? "up" : "down"} within ${ms}ms`);
}

/**
 * Kill whatever holds the bench port. Blunt on purpose: the port is ours alone.
 *
 * `xargs -r` is GNU-only — on macOS it is an illegal option, so the pipeline failed
 * before killing anything and the wait below then timed out on a shutdown that had
 * never been requested. It cost a sweep, and it was invisible because the whole
 * pipeline's output was discarded. Hence `while read`, and hence waiting
 * synchronously for the kill rather than firing it off unref'd.
 */
export async function stop(): Promise<void> {
  if (!await isUp()) return;
  await new Promise<void>((resolve) => {
    const child = spawn("sh", [
      "-c",
      `lsof -ti tcp:${PORT} | while read pid; do kill "$pid"; done`,
    ], { stdio: "ignore" });
    child.on("close", () => resolve());
  });
  await waitUntil(false, 10_000);
}

/**
 * Start the bench server with tracing on, and resolve once it answers. The trace
 * directory is passed here rather than per turn — it is a property of this whole
 * process, and every turn it runs is part of the experiment.
 */
export async function start(): Promise<void> {
  if (process.env["AHE_KEEP_SERVER"] === "1" && await isUp()) return;
  await stop();
  mkdirSync(HOME, { recursive: true });
  mkdirSync(TRACE_DIR, { recursive: true });
  const child = spawn("bun", ["src/server/main.ts"], {
    cwd: REPO,
    env: {
      ...process.env,
      BOUGH_HOME: HOME,
      BOUGH_PORT: String(PORT),
      BOUGH_TRACE_DIR: TRACE_DIR,
    },
    stdio: "ignore",
    detached: true,
  });
  child.unref();
  await waitUntil(true);
}
