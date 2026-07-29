/**
 * The per-session scratchpad: where a turn's temporary files go instead of the
 * user's checkout.
 *
 * THE PROBLEM IT SOLVES, stated as the failure rather than the feature: a program
 * that needs somewhere to put a build log, an intermediate JSON blob or a
 * throwaway script has exactly two places to reach for, and both are wrong. The
 * WORKSPACE is the user's real checkout — anything written there lands in the
 * changes rail and has to be read and reverted by a human who did not ask for it.
 * `/tmp` is shared with every other conversation, every other tool and every
 * leftover from last week: two sessions both write `build.log`, one clobbers the
 * other, and a later `find /tmp -mmin -5` matches a third program's debris and gets
 * read as this task's own output.
 *
 * WHY NOT UNDER `/tmp` AT ALL, which is the obvious answer and the one this
 * rejects: macOS empties `/tmp` on reboot and systemd-tmpfiles reaps entries older
 * than ten days. A conversation that spans either loses the directory it was told to
 * use, and the failure surfaces as a missing file in a turn that did nothing wrong.
 * `~/.bough` is where everything else a session owns already lives, and it is not
 * swept by anyone but us.
 *
 * SO IT MUST BE SWEPT BY US. A scratch root that only ever grows is the other half
 * of the same bug, and there is no session-delete verb to hang cleanup on (sessions
 * are append-only by design — `db/db.ts`). Age is the honest criterion: a directory
 * nothing has touched in `MAX_AGE_MS` belongs to a conversation nobody is coming
 * back to. Swept at boot, best-effort, never on the path of a turn.
 */
import { mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { scratchDirFor, scratchRoot } from "./paths.ts";

/**
 * How long an untouched scratch directory is kept.
 *
 * Two weeks rather than a day or two: the cost of keeping one is a few kilobytes,
 * and the cost of deleting one early is a file someone came back for. Long enough
 * to span a holiday, short enough that the root does not become an archive.
 */
export const MAX_AGE_MS = 14 * 24 * 60 * 60_000;

/**
 * This session's scratch directory, created if it is not there.
 *
 * Called before the prompt names the path: a directory the model is told to use and
 * that does not exist is worse than no scratchpad at all, because the first write
 * fails in a way that reads as the harness being broken.
 *
 * Never throws. An unwritable `~/.bough` is a real problem but not this function's
 * to raise — the turn can still run, and every other write path reports its own
 * failure in terms the reader can act on.
 */
export function ensureScratchDir(sessionId: string): string {
  const dir = scratchDirFor(sessionId);
  try {
    mkdirSync(dir, { recursive: true });
  } catch {
    // Reported by whatever writes next, in its own terms.
  }
  return dir;
}

export interface SweepOptions {
  /** Absent = `MAX_AGE_MS`. */
  maxAgeMs?: number;
  /** Injected clock, epoch ms. */
  now?: () => number;
  /** Absent = the real root. Tests pass their own. */
  root?: string;
}

/**
 * Delete scratch directories nothing has touched recently. Returns what went.
 *
 * MTIME OF THE DIRECTORY, not of the session row: a conversation can be months old
 * and still be the one you are working in, and the question here is whether anything
 * has been WRITTEN lately. Reading the row would also couple this to the database
 * for no gain — the id in the name is not needed to answer "is this stale".
 */
export function sweepScratch(opts: SweepOptions = {}): string[] {
  const root = opts.root ?? scratchRoot();
  const now = (opts.now ?? Date.now)();
  const maxAge = opts.maxAgeMs ?? MAX_AGE_MS;
  let entries: string[];
  try {
    entries = readdirSync(root);
  } catch {
    return []; // no root yet: nothing has ever been written, nothing to sweep
  }
  const removed: string[] = [];
  for (const name of entries) {
    const dir = join(root, name);
    try {
      const st = statSync(dir);
      if (!st.isDirectory() || now - st.mtimeMs <= maxAge) continue;
      rmSync(dir, { recursive: true, force: true });
      removed.push(name);
    } catch {
      // A directory that vanished under us, or one we may not read. Either way it
      // is not this sweep's business to complain about.
    }
  }
  return removed;
}
