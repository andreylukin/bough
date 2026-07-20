import { palette } from "../theme.ts";
import { useEffect, useRef, useState } from "react";
import { Box, Text } from "ink";
import type { Usage } from "../api.ts";
import { coldCacheNote, ctxPctLeft, fmtTokens, fmtUsd } from "../format.ts";
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
    draftLabel,
    model,
    parentTitle,
  }: {
    connected: boolean;
    busy: boolean;
    session: TuiSession | null;
    pendingCount: number;
    quitHint: boolean;
    mode: UiMode;
    usage: Usage;
    /** Shown instead of a session title while no session exists yet. */
    draftLabel?: string | null;
    /** Active model's label (global, from /config). */
    model?: string | null;
    /** Spawner's title when the open session is a subagent branch. */
    parentTitle?: string | null;
  },
) {
  const isSub = session?.kind === "subagent";
  const spinner = useSpinner(busy, isSub ? "esc ↩ back" : "esc interrupts");
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
          <Text wrap="truncate">
            <Text color={connected ? palette.accent : palette.error}>{connected ? "●" : "○"}</Text>
            <Text dimColor>{connected ? "" : " reconnecting…"}</Text>
            {session?.kind === "subagent" ? <Text color={palette.accent}>{" "}◆</Text> : null}
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
            {parentTitle ? <Text dimColor>{"  "}branch of {parentTitle} · esc ↩ back</Text> : null}
          </Text>
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
                    {"  "}{fmtTokens(usage.contextTokens)} ctx
                    {pctLeft !== null ? ` · ${pctLeft}% left` : ""}
                  </Text>
                )
              : null}
            {spend > 0 ? <Text dimColor>{"  "}{fmtUsd(spend)}</Text> : null}
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
          : mode === "chat" || mode === "panel"
          ? "? help"
          : ""}
      </Text>
    </Box>
  );
}
