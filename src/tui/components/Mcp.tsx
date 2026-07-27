/**
 * The MCP tab: which servers exist, which this session may call, and which are live.
 *
 * THE INVARIANT THIS HOLDS: **the three states are never conflated.** A server can be
 * *registered* (a row in the registry), *granted* to this session, and *connected* —
 * and they are independent. Registering is not granting (`mcp/config.ts`); a granted
 * server whose command is broken is granted and dead; an authorized remote server is
 * not necessarily connected. The old client showed one dot and one word, so "why can't
 * the agent call this" had no answer on the screen. Here every row carries all four
 * facts: grant, live tool count, authorization, and transport.
 *
 * SECOND — **nothing here is cached.** The status object is a prop, re-fetched by the
 * host on every entry into this tab, because grants and connections change between
 * turns (plan §6.13) and a panel showing last minute's MCP state is worse than one
 * showing none. This component keeps no state of its own to make that impossible.
 *
 * Split out of `Panel.tsx` so the panel file is chrome and a state machine; the tab
 * bodies are their own files with their own props.
 */
import { Box, Text } from "ink";
import type { McpStatus } from "../../mcp/status.ts";
import { clip } from "../format.ts";
import { palette } from "../theme.ts";

export interface McpTabProps {
  /** `null` while loading. Never cached by the caller (plan §6.13). */
  status: McpStatus | null;
  selected: number;
  message?: string | null;
}

/** The dim tail of an MCP row: grant, live connection, credentials, transport. */
export function mcpDetail(status: McpStatus, name: string): string {
  const granted = status.active.includes(name);
  const conn = status.connections.find((c) => c.server === name);
  const auth = status.auth[name];
  const entry = status.registry.servers[name];
  return [
    granted ? "granted" : "off",
    conn?.alive ? `${conn.toolCount} tools` : null,
    conn?.error ? clip(conn.error, 40) : null,
    auth ? (auth.authorized ? "authed" : "needs auth") : null,
    clip(entry.url ?? entry.command ?? "", 30),
  ].filter(Boolean).join(" · ");
}

export function McpTab({ status, selected, message }: McpTabProps) {
  if (!status) return <Text dimColor>loading…</Text>;
  const names = Object.keys(status.registry.servers).sort();
  if (names.length === 0) return <Text dimColor>no MCP servers configured</Text>;
  return (
    <Box flexDirection="column">
      {names.map((name, i) => {
        const granted = status.active.includes(name);
        const alive = status.connections.find((c) => c.server === name)?.alive;
        const sel = i === selected;
        const color = alive ? palette.accent : granted ? palette.warn : undefined;
        return (
          <Text key={name} wrap="truncate">
            <Text color={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</Text>
            <Text color={sel ? undefined : color} dimColor={!granted}>
              {alive ? "●" : granted ? "◐" : "○"}
            </Text>
            <Text bold={sel}>{" "}{name}</Text>
            <Text dimColor>{"  "}{mcpDetail(status, name)}</Text>
          </Text>
        );
      })}
      {message ? <Text color={palette.warn} wrap="wrap">{message}</Text> : null}
      <Text dimColor wrap="truncate">↑↓ move · ⏎ grant/revoke</Text>
    </Box>
  );
}
