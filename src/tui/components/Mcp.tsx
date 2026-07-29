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
import { isCoveredHost } from "../../mcp/keychain.ts";
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
  /**
   * The server URL being typed, or `null` when the buffer is closed.
   *
   * Registration used to mean hand-editing `~/.bough/mcp.json` and restarting the
   * server, which is why this tab's legend was one verb long.
   */
  entry?: string | null;
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

/**
 * The entry carries its own credential — a static `Authorization` header, which
 * `expandHeaders` resolves (from `${VAR}` or the keychain) at connect time.
 *
 * Any auth-bearing header counts, not only a keychain reference: a server given a
 * literal or an env-var token is equally not waiting for anyone to press `a`.
 */
export function hasStaticAuth(
  entry?: { headers?: Record<string, string> },
): boolean {
  return Object.entries(entry?.headers ?? {}).some(
    ([k, v]) => k.toLowerCase() === "authorization" && v.trim() !== "",
  );
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
    // "needs auth" is about tokens BOUGH stored, and saying it of a server that
    // already carries a credential is the lie this row used to tell: `sync-mcp`
    // writes an `Authorization` header referencing the grant Claude Code holds, and
    // the panel still said "needs auth" — so the one server that needed nothing
    // pressed was the one the UI sent you to press `a` on, where authorizing fails
    // because the provider does not do dynamic registration.
    //
    // Three states, in order of how the connection will actually be made: a token
    // bough stored; an explicit header on the entry; the machine's Claude Code
    // credential for a host it covers (`mcp/keychain.ts`). All derived from the
    // entry alone — the panel must not spawn a keychain read to paint a row — so
    // this says what will be TRIED, not that the server has already accepted it.
    auth
      ? auth.authorized
        ? "authed"
        : hasStaticAuth(entry)
        ? "keychain"
        : isCoveredHost(entry.url ?? "")
        ? "keychain"
        : "needs auth"
      : null,
    clip(entry.url ?? entry.command ?? "", 30),
  ].filter(Boolean).join(" · ");
}

export function McpTab({ status, selected, message, rows = 20, entry = null }: McpTabProps) {
  if (!status) return <text attributes={TextAttributes.DIM}>loading…</text>;
  const names = Object.keys(status.registry.servers).sort();
  const legend = (
    <text attributes={TextAttributes.DIM} wrapMode="none">
      {entry === null
        ? "↑↓ move · 1-9 pick · ⏎ grant/revoke · c test · a authorize · n add · F forget · d delete · esc back"
        : "⏎ registers · ⌫ back · esc cancels"}
    </text>
  );
  // The prompt REPLACES the list's affirmative while it is open — see `confirm`,
  // which takes ⏎ before any tab does.
  const prompt = entry === null ? null : (
    <text wrapMode="none">
      <span attributes={TextAttributes.DIM}>new server </span>
      <span>{entry}</span>
      <span fg={palette.accent}>▌</span>
    </text>
  );
  if (names.length === 0) {
    return (
      <box flexDirection="column">
        {prompt}
        <text attributes={TextAttributes.DIM}>
          {entry === null ? "no MCP servers configured — n adds one by URL" : ""}
        </text>
        {legend}
      </box>
    );
  }
  const { start, end, height, counter } = mcpWindow(
    names.length,
    selected,
    rows,
    (message ? 1 : 0) + (prompt ? 1 : 0),
  );
  return (
    <box flexDirection="column">
      {prompt}
      {/* NOT clipped to one line when it carries a URL: an authorization URL that
          ends in "…" is a URL nobody can open, which is the whole point of it. */}
      {message
        ? (
          <text fg={palette.warn} wrapMode={message.includes("://") ? "word" : "none"}>
            {message.includes("://") ? message : clip(message, 96)}
          </text>
        )
        : null}
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
