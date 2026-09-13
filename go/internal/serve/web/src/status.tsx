import type { Row, Status } from "./types";

/**
 * One attention model for every surface: 0 wants a person (a recorded
 * failure, seen or not, or a pending request), 1 is moving, 2 is resting.
 * Seen only changes how loud a row looks, never where it ranks.
 */
export function sessionSignal(r: Row): 0 | 1 | 2 {
  return r.trouble || r.testsFailed || r.status === "needs-you" ? 0 : r.status === "running" ? 1 : 2;
}

/**
 * How each status looks. Colour is never the only carrier: every state
 * pairs a distinct glyph with a word, so it survives a colourblind
 * reader, a greyscale screenshot and a print.
 *
 * Only two states get a hue. Amber means exactly "waiting for you" and
 * red means exactly "failed" — the accent green is reserved for
 * interactive things, so a green "done" would have made the accent
 * ambiguous. Resting states are neutral and need no colour at all.
 */
export const STATUS: Record<Status, { label: string; tone: string; glyph: React.ReactNode }> = {
  "needs-you": {
    label: "Waiting for you",
    tone: "var(--amber)",
    glyph: (
      <>
        <circle cx="12" cy="12" r="8.5" />
        <path d="M12 8v4.5l3 2" />
      </>
    ),
  },
  running: {
    label: "Running",
    tone: "var(--text-2)",
    glyph: <circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" />,
  },
  done: {
    label: "Done",
    tone: "var(--text-3)",
    glyph: <path d="M4 12.5l5 5L20 6.5" />,
  },
  stopped: {
    label: "Stopped",
    tone: "var(--text-3)",
    glyph: <rect x="6" y="6" width="12" height="12" rx="1.5" />,
  },
  error: {
    label: "Failed",
    tone: "var(--red)",
    glyph: (
      <>
        <path d="M12 4.5L21 19H3z" />
        <path d="M12 10v4" />
        <path d="M12 16.5h.01" />
      </>
    ),
  },
  interrupted: {
    label: "Interrupted",
    tone: "var(--text-2)",
    glyph: <path d="M9 6.5v11M15 6.5v11" />,
  },
  idle: {
    label: "Idle",
    tone: "var(--text-3)",
    glyph: <circle cx="12" cy="12" r="3.5" />,
  },
};

/** `bare` drops the word where a neighbour already says it ("Waiting 8d"); it stays for screen readers. */
export function StatusMark({ status, size = 13, bare }: { status: Status; size?: number; bare?: boolean }) {
  const s = STATUS[status] ?? STATUS.idle;
  // The one state that is still changing gets a moving mark. It turns
  // because the session is running, never on a timer of its own, so it
  // stops the moment the derived status stops saying "running".
  // Under prefers-reduced-motion the stylesheet freezes the rotation
  // and what is left is a gapped ring beside the word "Running" —
  // still a distinct glyph, still labelled.
  const spin = status === "running";
  return (
    <span className="status" style={{ display: "inline-flex", alignItems: "center", gap: 7, color: s.tone }}>
      <svg
        className={spin ? "spin-mark" : undefined}
        width={size}
        height={size}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        {s.glyph}
      </svg>
      <span className={bare ? "visually-hidden" : undefined} style={{ fontSize: 12 }}>{s.label}</span>
    </span>
  );
}

/**
 * The running banner at the foot of a live transcript. The sidebar row
 * says a session is running; inside the thread you are watching one
 * session and the question is whether *this* is still going, so it is
 * said again where the next words will appear.
 *
 * `label` says what it is doing when that is known — the composer is
 * the only other place that has to admit a turn is already under way.
 */
export function Working({ label = "Working" }: { label?: string }) {
  return (
    <p className="working" role="status">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9"
           strokeLinecap="round" className="spin-mark" aria-hidden="true">
        <circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" />
      </svg>
      <span>{label}</span>
      {/* Three dots that breathe, so the line is alive even at a glance
          from across a desk. Frozen under reduced motion, where the
          word alone carries it. */}
      <span className="working-dots" aria-hidden="true"><i /><i /><i /></span>
    </p>
  );
}
