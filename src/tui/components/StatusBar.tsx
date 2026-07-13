import { Box, Text } from "ink";
import type { Usage } from "../api.ts";
import { fmtTokens } from "../format.ts";
import { HINTS, type UiMode } from "../keys.ts";
import type { TuiSession } from "../store.ts";

export function StatusBar(
  { connected, busy, session, pendingCount, quitHint, mode, usage, draftLabel }: {
    connected: boolean;
    busy: boolean;
    session: TuiSession | null;
    pendingCount: number;
    quitHint: boolean;
    mode: UiMode;
    usage: Usage;
    /** Shown instead of a session title while no session exists yet. */
    draftLabel?: string | null;
  },
) {
  const status = busy
    ? { text: "⋯ working", color: "yellow" }
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
        {session
          ? <Text bold>{" "}{session.title || "(untitled)"}</Text>
          : draftLabel
          ? <Text dimColor>{" "}{draftLabel}</Text>
          : null}
        {status ? <Text color={status.color}>{"  "}{status.text}</Text> : null}
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
