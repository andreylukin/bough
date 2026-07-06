// A shared coarse clock for time-decaying UI (prompt-cache warmth). Nothing visible
// counts down — the indicator only appears/disappears at the warm/cold boundary and
// hover text is minute-granular — so a 10s tick is plenty. One module-level interval
// fans out to every subscriber; it only runs while someone is mounted.
import { useEffect, useState } from "react";

const subs = new Set<(t: number) => void>();
let timer: number | undefined;

function subscribe(fn: (t: number) => void): () => void {
  subs.add(fn);
  if (timer === undefined) {
    timer = window.setInterval(() => {
      const t = Date.now();
      for (const s of subs) s(t);
    }, 10_000);
  }
  return () => {
    subs.delete(fn);
    if (subs.size === 0 && timer !== undefined) {
      window.clearInterval(timer);
      timer = undefined;
    }
  };
}

/** The current epoch ms, re-rendering every ~10s (coarse — for warm/cold boundaries). */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => subscribe(setNow), []);
  return now;
}
