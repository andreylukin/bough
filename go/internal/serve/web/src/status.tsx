import type { Status } from "./types";

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
    glyph: (
      <>
        <path d="M12 4.5L21 19H3z" />
        <path d="M12 10v4" />
        <path d="M12 16.5h.01" />
      </>
    ),
  },
  idle: {
    label: "Idle",
    tone: "var(--text-3)",
    glyph: <circle cx="12" cy="12" r="3.5" />,
  },
};

export function StatusMark({ status, size = 13 }: { status: Status; size?: number }) {
  const s = STATUS[status] ?? STATUS.idle;
  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: 7, color: s.tone }}>
      <svg
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
      <span style={{ fontSize: 12 }}>{s.label}</span>
    </span>
  );
}
