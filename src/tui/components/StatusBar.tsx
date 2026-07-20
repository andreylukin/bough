import { palette } from "../theme.ts";
import { useEffect, useRef, useState } from "react";
import { Box, Text } from "ink";
import type { Usage } from "../api.ts";
import { coldCacheNote, fmtTokens } from "../format.ts";
import { HINTS, PANEL_HINTS, type UiMode } from "../keys.ts";
import type { PanelTab } from "./Panel.tsx";
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

/** Animated spinner + elapsed seconds while a turn runs. */
function useSpinner(busy: boolean): string | null {
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
  return `${FRAMES[tick % FRAMES.length]} working · ${secs}s · esc interrupts`;
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
    panelTab,
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
    /** Which panel tab is showing (drives the per-tab hint while mode is "panel"). */
    panelTab: PanelTab;
    usage: Usage;
    /** Shown instead of a session title while no session exists yet. */
    draftLabel?: string | null;
    /** Active model's label (global, from /config). */
    model?: string | null;
    /** Spawner's title when the open session is a subagent branch. */
    parentTitle?: string | null;
  },
) {
  const spinner = useSpinner(busy);
  const cold = useColdCache(usage);
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
            {parentTitle
              ? <Text dimColor>{"  "}branch of {parentTitle} · ^p goes back</Text>
              : null}
          </Text>
        </Box>
        <Box flexShrink={0}>
          <Text>
            {status ? <Text color={status.color}>{"  "}{status.text}</Text> : null}
            {model ? <Text dimColor>{"  "}⌬ {model}</Text> : null}
            {usage.contextTokens > 0
              ? cold && !busy
                ? <Text color={palette.warn}>{"  "}{cold}</Text>
                : <Text dimColor>{"  "}{fmtTokens(usage.contextTokens)} ctx</Text>
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
      <Text dimColor wrap="truncate">
        {quitHint ? "ctrl+c again to quit" : mode === "panel" ? PANEL_HINTS[panelTab] : HINTS[mode]}
      </Text>
    </Box>
  );
}
