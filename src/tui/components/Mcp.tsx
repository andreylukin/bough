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
import { TextAttributes } from "@opentui/core";
import type { McpStatus } from "../../mcp/status.ts";
import { clip, windowAround } from "../format.ts";
import { palette } from "../theme.ts";

export interface McpTabProps {
  /** `null` while loading. Never cached by the caller (plan §6.13). */
  status: McpStatus | null;
  selected: number;
  message?: string | null;
  /**
   * Rows this tab may paint. It had NONE — it listed every registered server and
   * then the legend, so an install with a dozen servers overran the panel and
   * OpenTUI shrank the rows onto each other (`Panel.tsx`).
   */
  rows?: number;
}

/** The visible slice. Chrome is the message and the legend, which is always last. */
export function mcpWindow(
  count: number,
  selected: number,
  rows: number,
  chrome = 0,
): { start: number; end: number; height: number; counter: boolean } {
  const avail = Math.max(0, rows - chrome - 1 /* legend */);
  // Content over indicators when it is tight — see `sessionsWindow`.
  const counter = count > avail && avail >= 2;
  const height = Math.max(0, avail - (counter ? 1 : 0));
  const { start, end } = windowAround(selected, count, height);
  return { start: Math.max(0, start), end, height, counter };
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

export function McpTab({ status, selected, message, rows = 20 }: McpTabProps) {
  if (!status) return <text attributes={TextAttributes.DIM}>loading…</text>;
  const names = Object.keys(status.registry.servers).sort();
  const legend = (
    <text attributes={TextAttributes.DIM} wrapMode="none">
      ↑↓ move · pgup/pgdn page · 1-9 pick · ⏎ grant/revoke · esc back
    </text>
  );
  if (names.length === 0) {
    return (
      <box flexDirection="column">
        <text attributes={TextAttributes.DIM}>no MCP servers configured</text>
        {legend}
      </box>
    );
  }
  const { start, end, height, counter } = mcpWindow(
    names.length,
    selected,
    rows,
    message ? 1 : 0,
  );
  return (
    <box flexDirection="column">
      {message ? <text fg={palette.warn} wrapMode="none">{clip(message, 96)}</text> : null}
      {(height === 0 ? [] : names.slice(start, end)).map((name, i) => {
        const idx = start + i;
        const granted = status.active.includes(name);
        const alive = status.connections.find((c) => c.server === name)?.alive;
        const sel = idx === selected;
        const color = alive ? palette.accent : granted ? palette.warn : undefined;
        return (
          <text key={name} wrapMode="none">
            <span attributes={TextAttributes.DIM}>{i < 9 ? `${i + 1} ` : "  "}</span>
            <span fg={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</span>
            <span
              fg={sel ? undefined : color}
              attributes={granted ? TextAttributes.NONE : TextAttributes.DIM}
            >
              {alive ? "●" : granted ? "◐" : "○"}
            </span>
            <span attributes={sel ? TextAttributes.BOLD : TextAttributes.NONE}>{" "}{name}</span>
            <span attributes={TextAttributes.DIM}>{"  "}{mcpDetail(status, name)}</span>
          </text>
        );
      })}
      {counter
        ? <text attributes={TextAttributes.DIM}>— {end}/{names.length} —</text>
        : null}
      {/* The legend is the tab's LAST row. This tab had none at all until the
          message row happened to be absent, and the message row is not a legend. */}
      {legend}
    </box>
  );
}
