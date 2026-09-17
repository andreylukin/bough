import type { ReactNode } from "react";
import type { OrbStatus, Row, Status } from "./types";

/**
 * One attention model for every surface: 0 wants a person (a recorded
 * failure, seen or not, or a pending request), 1 is moving, 2 is resting.
 * Seen only changes how loud a row looks, never where it ranks.
 */
export function sessionSignal(r: Row): 0 | 1 | 2 {
  return hasFailure(r) || hasQuestion(r) ? 0 : r.status === "running" ? 1 : 2;
}

/** A failure older than this, already marked seen, is history, not a to-do. */
const STALE_FAILURE_MS = 72 * 3_600_000;

/**
 * Red: an unseen failure, or a recorded one (failed tests, an error) from
 * the last three days. Old failures that were already seen used to crowd
 * the recent groups with week-old benchmark runs.
 */
export function hasFailure(r: Row): boolean {
  if (r.trouble) return true;
  const recent = !r.lastAt || Date.now() - Date.parse(r.lastAt) < STALE_FAILURE_MS;
  return Boolean((r.testsFailed || r.status === "error") && recent);
}

/** The status a list shows: a recorded failure outranks "Done", as in the sidebar. */
export function shownStatus(r: Row): Status {
  return hasFailure(r) && !hasQuestion(r) ? "error" : r.status;
}

/** Amber: the session is waiting on an answer. */
export function hasQuestion(r: Row): boolean {
  return r.status === "needs-you" || Boolean(r.ask);
}

/**
 * How each status looks. Colour is never the only carrier: every state
 * pairs a distinct glyph with a word, so it survives a colourblind
 * reader, a greyscale screenshot and a print.
 *
 * Amber means exactly "waiting for you", red means exactly "failed", and
 * the accent marks the one live state, running. Done and every resting
 * state are neutral. Glyphs draw in currentColor at a 1.5 stroke.
 */
export const TESTS_FAILED_GLYPH = (
  <>
    <path d="M12 4.5L21 19H3z" />
    <path d="M12 10v4" />
    <path d="M12 16.5h.01" />
  </>
);

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
    tone: "var(--accent)",
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
    // x-circle; tests failed wears the triangle (TESTS_FAILED_GLYPH) so the icon column alone tells them apart.
    glyph: (
      <>
        <circle cx="12" cy="12" r="8.5" />
        <path d="M9 9l6 6M15 9l-6 6" />
      </>
    ),
  },
  interrupted: {
    label: "Interrupted",
    tone: "var(--text-2)",
    glyph: <path d="M9 6.5v11M15 6.5v11" />,
  },
  // Hollow clock, dashed: waiting its turn for a slot, not waiting on you.
  queued: {
    label: "Queued",
    tone: "var(--text-3)",
    glyph: (
      <>
        <circle cx="12" cy="12" r="8.5" strokeDasharray="3 3" />
        <path d="M12 8.5v3.5l2.5 1.5" />
      </>
    ),
  },
  idle: {
    label: "Idle",
    tone: "var(--text-3)",
    glyph: <circle cx="12" cy="12" r="3.5" />,
  },
};

/**
 * The one vocabulary. Every surface that names a state — sidebar, header,
 * turn footers, palette, projects, the orb table — reads its word here,
 * so a finished turn and a finished session both say "Done".
 */
export function statusWord(s: Status): string {
  return (STATUS[s] ?? STATUS.idle).label;
}

const ORB_WORD: Record<OrbStatus, string> = {
  "": "Pending", building: "Building", starting: "Starting", running: "Running", stopped: "Stopped", failed: "Setup failed",
};

/** An orb's state in the same sentence case as a session's. */
export function orbWord(s: OrbStatus): string {
  return ORB_WORD[s] ?? s;
}

/**
 * A project's environment that could not be set up. It is the orb that
 * failed, not the session, so it says whose setup it was and wears its
 * own mark, apart from the run status beside it.
 */
export function SetupFailed({ name }: { name: string }) {
  return (
    <span className="status setup-failed" title={`${name}: setup failed`}>
      <svg width={12} height={12} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.9"
           strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">{STATUS.error.glyph}</svg>
      <span>Setup failed</span>
    </span>
  );
}

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
        strokeWidth="1.5"
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
export function Working({ label = "Working", children }: { label?: string; /** Trailing detail, e.g. the live elapsed time. */ children?: ReactNode }) {
  return (
    <p className="working" role="status">
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"
           strokeLinecap="round" className="spin-mark" aria-hidden="true">
        <circle cx="12" cy="12" r="8.5" strokeDasharray="40 14" />
      </svg>
      <span>{label}</span>
      {/* The spinner carries liveness; the elapsed time follows after the gap. */}
      {children && <span className="working-after">{children}</span>}
    </p>
  );
}
