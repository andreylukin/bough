import { palette } from "../theme.ts";
import { useEffect, useRef, useState } from "react";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { Usage } from "../api.ts";
import { coldCacheNote, ctxPctLeft, disconnectNote, fmtTokens, fmtUsd } from "../format.ts";
import type { UiMode } from "../keys.ts";
import type { TuiSession } from "../store.ts";

const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/** Cold-cache resume warning, re-evaluated on a slow tick so it appears while
 * the user idles (staleness is a function of wall clock, not of events). */
function useColdCache(usage: Usage): string | null {
  const [, setTick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setTick((x) => x + 1), 30_000);
    return () => clearInterval(t);
  }, []);
  return coldCacheNote(usage, Date.now());
}

/** Tracks how long the event stream has been down and ticks once a second while
 * disconnected, so the chip escalates from "reconnecting…" to a server-unreachable
 * line whose elapsed time counts up. Reconnect resets it (App resyncs state). */
function useDisconnectNote(connected: boolean): { text: string; urgent: boolean } | null {
  const since = useRef<number | null>(null);
  const [, setTick] = useState(0);
  useEffect(() => {
    if (connected) {
      since.current = null;
      return;
    }
    since.current ??= Date.now();
    const t = setInterval(() => setTick((x) => x + 1), 1000);
    return () => clearInterval(t);
  }, [connected]);
  if (connected) return null;
  return disconnectNote(since.current ?? Date.now(), Date.now());
}

/** Animated spinner + elapsed seconds while a turn runs. `escHint` closes the
 * line — "esc interrupts" for your own turn, "esc ↩ back" inside a subagent
 * branch (where esc returns to the spawner instead of killing the subagent). */
function useSpinner(busy: boolean, escHint: string): string | null {
  const [tick, setTick] = useState(0);
  const since = useRef<number | null>(null);
  useEffect(() => {
    if (!busy) {
      since.current = null;
      return;
    }
    since.current ??= Date.now();
    const t = setInterval(() => setTick((x) => x + 1), 120);
    return () => clearInterval(t);
  }, [busy]);
  if (!busy) return null;
  const secs = Math.floor((Date.now() - (since.current ?? Date.now())) / 1000);
  return `${FRAMES[tick % FRAMES.length]} working · ${secs}s · ${escHint}`;
}

/** The line above the composer while a turn runs — the local worker's blurb of
 * what the program is doing, Claude-Code-style ("✳ Running the test suite…"). */
export function ActivityLine({ text }: { text: string }) {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setTick((x) => x + 1), 120);
    return () => clearInterval(t);
  }, []);
  return (
    <Text color={palette.warn} wrap="truncate">
      {FRAMES[tick % FRAMES.length]} {text}…
    </Text>
  );
}

export function StatusBar(
  {
    connected,
    busy,
    session,
    pendingCount,
    quitHint,
    mode,
    usage,
    bgJobs,
    draftLabel,
    dir,
    model,
    parentTitle,
    composerEmpty = true,
  }: {
    connected: boolean;
    busy: boolean;
    session: TuiSession | null;
    pendingCount: number;
    quitHint: boolean;
    mode: UiMode;
    usage: Usage;
    /** Running background shells in this session (incl. its subagents). */
    bgJobs?: number;
    /** Shown instead of a session title while no session exists yet. */
    draftLabel?: string | null;
    /** The open session's project dir (~-shortened) — which project this is. */
    dir?: string | null;
    /** Active model's label (global, from /config). */
    model?: string | null;
    /** Spawner's title when the open session is a subagent branch. */
    parentTitle?: string | null;
    /** The composer is empty — the `?`→help chord only fires then, so the hint
     * is suppressed while there's text (a lone "?" would still route to help). */
    composerEmpty?: boolean;
  },
) {
  const isSub = session?.kind === "subagent";
  const spinner = useSpinner(busy, isSub ? "esc/← ↩ back" : "esc interrupts");
  const down = useDisconnectNote(connected);
  const cold = useColdCache(usage);
  // Session spend: tree rollup (incl. subagents) when present, else own total.
  const spend = usage.tree?.costUsd ?? usage.costUsd ?? 0;
  // Usable context left; the chip turns warn-colored when the window runs low.
  const pctLeft = ctxPctLeft(usage);
  const ctxLow = pctLeft !== null && pctLeft <= 10;
  const status = spinner
    ? { text: spinner, color: palette.warn }
    : session?.lastTurnStatus === "error"
    ? { text: "✗ error", color: palette.error }
    : session?.lastTurnStatus === "interrupted"
    ? { text: "◼ interrupted", color: palette.warn }
    : session?.lastTurnStatus === "done"
    ? { text: "✓", color: palette.accent }
    : null;
  return (
    <Box justifyContent="space-between" gap={2}>
      {
        /* Title shrinks; the status cluster (spinner/esc-interrupts, holds) never
        does — a long auto-title must not truncate away the brake hint (probe
        finding: "esc interrupts" vanished behind a story-length title). */
      }
      <Box minWidth={0} flexShrink={1}>
        <Box minWidth={4} flexShrink={1}>
          <Box flexShrink={0}>
            <Text>
              <Text color={connected ? palette.accent : palette.error}>{connected ? "●" : "○"}</Text>
              {down
                ? (
                  <Text color={down.urgent ? palette.error : undefined} dimColor={!down.urgent}>
                    {" "}
                    {down.text}
                  </Text>
                )
                : null}
            </Text>
          </Box>
          {
            /* Inside a subagent the title is a breadcrumb — ● parent › ◆ sub — so
            the thread visibly hangs under its spawner. The parent crumb shrinks
            first (higher flexShrink) so the subagent title survives narrow widths. */
          }
          {isSub && parentTitle
            ? (
              <>
                <Box flexShrink={10} minWidth={2}>
                  <Text wrap="truncate" dimColor>{" "}{parentTitle}</Text>
                </Box>
                <Box flexShrink={0}>
                  <Text dimColor>{" ›"}</Text>
                </Box>
              </>
            )
            : null}
          <Box flexShrink={1} minWidth={0}>
            <Text wrap="truncate">
              {/* Location cue, not a success mark — info blue, distinct from a done card's accent ◆. */}
              {isSub ? <Text color={palette.info}>{" "}◆</Text> : null}
              {session
                ? (
                  <Text bold>
                    {" "}
                    {(session.title || "(untitled)").replace(/^subagent · /, "")}
                  </Text>
                )
                : draftLabel
                ? <Text dimColor>{" "}{draftLabel}</Text>
                : null}
              {session && dir ? <Text dimColor>{"  "}{dir}</Text> : null}
              {isSub && parentTitle ? <Text dimColor>{"  "}esc/← ↩ back</Text> : null}
            </Text>
          </Box>
        </Box>
        <Box flexShrink={0}>
          <Text>
            {status ? <Text color={status.color}>{"  "}{status.text}</Text> : null}
            {model ? <Text dimColor>{"  "}⌬ {model}</Text> : null}
            {usage.contextTokens > 0
              ? cold && !busy
                ? <Text color={palette.warn}>{"  "}{cold}</Text>
                : (
                  <Text dimColor={!ctxLow} color={ctxLow ? palette.warn : undefined}>
                    {"  "}
                    {fmtTokens(usage.contextTokens)} ctx
                    {pctLeft !== null ? ` · ${pctLeft}% left` : ""}
                  </Text>
                )
              : null}
            {/* Cost carries its own weight so it doesn't read as more gray
                metadata — bold lifts it out of the dim chip run. */}
            {spend > 0 ? <Text bold>{"  "}{fmtUsd(spend)}</Text> : null}
            {/* The chip names its own key: a count with no way to act on it was
                the whole complaint about background jobs. */}
            {(bgJobs ?? 0) > 0
              ? <Text color={palette.warn}>{"  "}⚙ {bgJobs} bg ^b</Text>
              : null}
            {pendingCount > 0
              ? (
                <Text color={palette.warn}>
                  {"  "}⏸ {pendingCount} hold{pendingCount > 1 ? "s" : ""}
                </Text>
              )
              : null}
          </Text>
        </Box>
      </Box>
      {
        /* The right side is only "? help" — keybindings live in the ? overlay,
        the hold card, and the modals; the status bar stays session info. */
      }
      <Text dimColor wrap="truncate">
        {quitHint
          ? "ctrl+c again to quit"
          : mode === "help"
          ? "any key closes"
          : (mode === "chat" || mode === "panel") && composerEmpty
          ? "? help"
          : ""}
      </Text>
    </Box>
  );
}
