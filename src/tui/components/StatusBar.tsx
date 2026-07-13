import { useEffect, useRef, useState } from "react";
import { Box, Text } from "ink";
import type { Usage } from "../api.ts";
import { fmtTokens } from "../format.ts";
import { HINTS, type UiMode } from "../keys.ts";
import type { TuiSession } from "../store.ts";

const FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

export function StatusBar(
  { connected, busy, session, pendingCount, quitHint, mode, usage, draftLabel, model, parentTitle }:
    {
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
  const spinner = useSpinner(busy);
  const status = spinner
    ? { text: spinner, color: "yellow" }
    : session?.lastTurnStatus === "error"
    ? { text: "✗ error", color: "red" }
    : session?.lastTurnStatus === "interrupted"
    ? { text: "◼ interrupted", color: "yellow" }
    : session?.lastTurnStatus === "done"
    ? { text: "✓", color: "green" }
    : null;
  return (
    <Box justifyContent="space-between" gap={2}>
      <Text wrap="truncate">
        <Text color={connected ? "green" : "red"}>{connected ? "●" : "○"}</Text>
        <Text dimColor>{connected ? "" : " reconnecting…"}</Text>
        {session?.kind === "subagent" ? <Text color="green">{" "}◆</Text> : null}
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
        {parentTitle ? <Text dimColor>{"  "}branch of {parentTitle} · ^p to go back</Text> : null}
        {status ? <Text color={status.color}>{"  "}{status.text}</Text> : null}
        {model ? <Text dimColor>{"  "}⌬ {model}</Text> : null}
        {usage.contextTokens > 0
          ? <Text dimColor>{"  "}{fmtTokens(usage.contextTokens)} ctx</Text>
          : null}
        {pendingCount > 0
          ? <Text color="yellow">{"  "}⏸ {pendingCount} hold{pendingCount > 1 ? "s" : ""}</Text>
          : null}
      </Text>
      <Text dimColor wrap="truncate">
        {quitHint ? "ctrl+c again to quit" : HINTS[mode]}
      </Text>
    </Box>
  );
}
