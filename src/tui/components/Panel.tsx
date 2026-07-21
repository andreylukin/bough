// The tabbed side-panel: net (Claw Patrol feed + policy), mcp (server registry),
// skills. App owns the data and key handling.
import { palette, THEME_PRESETS } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { NetRequest } from "../../schema/parts.ts";
import type { McpStatus, NetConfig, NetStatus, SkillInfo, ThemeState } from "../api.ts";
import { clip, relTime } from "../format.ts";

// The management view is ONE tabbed panel — every non-chat surface is a tab:
// session tree, conversation branch tree, changes review, model/keys, and the
// net/mcp/skills/theme tabs. ^t toggles it; ^p/^f/^d/^o jump to a tab.
export type PanelTab =
  | "sessions"
  | "conversation"
  | "changes"
  | "model"
  | "net"
  | "mcp"
  | "skills"
  | "theme";
export const PANEL_TABS: PanelTab[] = [
  "sessions",
  "conversation",
  "changes",
  "model",
  "net",
  "mcp",
  "skills",
  "theme",
];

/** The tab bar for the unified panel — active tab bold + green underline. */
export function PanelTabs({ tab }: { tab: PanelTab }) {
  return (
    <Text>
      {PANEL_TABS.map((t, i) => (
        <Text key={t}>
          {i > 0 ? "   " : ""}
          <Text
            bold={t === tab}
            color={t === tab ? palette.accent : undefined}
            dimColor={t !== tab}
          >
            {t}
          </Text>
        </Text>
      ))}
    </Text>
  );
}

// A function, not a const map: palette values change when a theme is applied
// mid-run, and a module-level map would freeze the boot palette.
const verdictColor = (v: NetRequest["verdict"]): string =>
  v === "allowed" ? palette.accent : v === "denied" ? palette.error : palette.warn;

function NetTab(
  { status, policy, feed, rows, scopeLabel }: {
    status: NetStatus | null;
    policy: NetConfig | null;
    feed: NetRequest[];
    rows: number;
    /** Which requests the feed is filtered to ("this session" / "all sessions"). */
    scopeLabel: string;
  },
) {
  if (!status) return <Text dimColor>loading…</Text>;
  const yolo = policy?.mode === "yolo";
  const visible = feed.slice(0, Math.max(3, rows - 12));
  return (
    <Box flexDirection="column">
      <Text>
        {status.enabled
          ? (
            <Text color={yolo ? palette.error : palette.accent}>
              {yolo ? "⚠ YOLO (log-only)" : "● gating"}
            </Text>
          )
          : <Text dimColor>○ off</Text>}
        {policy
          ? (
            <Text dimColor>
              {"  "}mode {policy.mode}
              {policy.prevMode ? ` (was ${policy.prevMode})` : ""} · miss {policy.hostMiss} ·{" "}
              {policy.allowHosts.length} allow / {policy.denyHosts.length} deny hosts ·{" "}
              {status.listeners} listener{status.listeners === 1 ? "" : "s"}
            </Text>
          )
          : null}
      </Text>
      {status.caTrusted === false
        ? <Text color={palette.warn} wrap="truncate">CA untrusted — {status.caTrustCommand}</Text>
        : null}
      <Box marginTop={1} flexDirection="column">
        <Text dimColor>{scopeLabel}</Text>
        {visible.length === 0
          ? <Text dimColor>no gated requests yet</Text>
          : visible.map((r) => (
            <Text key={r.id} wrap="truncate">
              <Text color={verdictColor(r.verdict)}>
                {r.verdict === "allowed" ? "✓" : r.verdict === "denied" ? "✗" : "⏸"}
              </Text>{" "}
              {r.verb ? <Text>{r.verb}{" "}</Text> : null}
              <Text bold>{r.host}</Text>
              {r.path && r.path !== "/" ? clip(r.path, 32) : ""}
              <Text dimColor>{"  "}{r.action}{"  "}{relTime(r.ts)} ago</Text>
            </Text>
          ))}
      </Box>
    </Box>
  );
}

function McpTab(
  { mcp, selected, msg }: { mcp: McpStatus | null; selected: number; msg: string | null },
) {
  if (!mcp) return <Text dimColor>loading…</Text>;
  const names = Object.keys(mcp.registry.servers).sort();
  if (names.length === 0) return <Text dimColor>no MCP servers configured — see /mcp</Text>;
  return (
    <Box flexDirection="column">
      {names.map((name, i) => {
        const active = mcp.active.includes(name);
        const conn = mcp.connections.find((c) => c.server === name);
        const auth = mcp.auth[name];
        const entry = mcp.registry.servers[name];
        return (
          <Text key={name} inverse={i === selected} wrap="truncate">
            <Text
              color={conn?.alive ? palette.accent : active ? palette.warn : undefined}
              dimColor={!active}
            >
              {conn?.alive ? "●" : active ? "◐" : "○"}
            </Text>{" "}
            {name}
            <Text dimColor>
              {"  "}
              {active ? "on" : "off"}
              {conn?.alive ? ` · ${conn.toolCount} tools` : ""}
              {auth ? (auth.authorized ? " · authed" : " · needs auth") : ""}{"  "}
              {entry.url ?? entry.command ?? ""}
            </Text>
          </Text>
        );
      })}
      {msg ? <Text color={palette.warn} wrap="wrap">{msg}</Text> : null}
    </Box>
  );
}

/** Swatch tokens shown per preset row, in order: surfaces, accent, text. */
const SWATCH_TOKENS = ["bg", "panel", "panelInset", "green", "text"];

// The theme tab previews live: moving the cursor PUTs the hovered preset and
// the whole TUI recolors. Enter keeps it; Escape restores the theme held on
// tab entry (App reverts), so browsing never commits.
function ThemeTab(
  { state, selected }: { state: ThemeState | null; selected: number },
) {
  if (!state) return <Text dimColor>loading theme…</Text>;
  const currentName = state.theme?.name ?? "Default";
  const isPreset = THEME_PRESETS.some((p) => p.name === currentName);
  return (
    <Box flexDirection="column">
      <Text dimColor>
        current: {currentName}
        {isPreset ? "" : " (custom)"} — ↑↓ preview · enter keep · esc revert
      </Text>
      {THEME_PRESETS.map((p, i) => {
        // The row's own resolved colors — distinct from the live TUI `palette`.
        const row = { ...state.defaults, ...p.colors };
        const active = currentName === p.name;
        return (
          <Text key={p.name} inverse={i === selected} wrap="truncate">
            <Text color={palette.accent}>{active ? "●" : " "}</Text> {p.name.padEnd(15)}{" "}
            {SWATCH_TOKENS.map((t) => <Text key={t} color={row[t]}>██</Text>)}
            <Text dimColor>{"  "}{p.note}</Text>
          </Text>
        );
      })}
    </Box>
  );
}

function SkillsTab({ skills, rows }: { skills: SkillInfo[] | null; rows: number }) {
  if (!skills) return <Text dimColor>loading…</Text>;
  if (skills.length === 0) return <Text dimColor>no skills installed</Text>;
  return (
    <Box flexDirection="column">
      {skills.slice(0, Math.max(3, rows - 8)).map((s) => (
        <Text key={s.name} wrap="truncate">
          <Text bold>{s.name}</Text>
          <Text dimColor>{"  "}{s.description}</Text>
        </Text>
      ))}
      {skills.length > rows - 8 ? <Text dimColor>… {skills.length - (rows - 8)} more</Text> : null}
    </Box>
  );
}

export function Panel(
  { tab, status, policy, feed, mcp, mcpSel, mcpMsg, skills, rows, netScopeLabel, theme, themeSel }:
    {
      tab: PanelTab;
      status: NetStatus | null;
      policy: NetConfig | null;
      feed: NetRequest[];
      mcp: McpStatus | null;
      mcpSel: number;
      mcpMsg: string | null;
      skills: SkillInfo[] | null;
      rows: number;
      netScopeLabel: string;
      theme: ThemeState | null;
      themeSel: number;
    },
) {
  // Content only — the unified panel container owns the border + tab bar.
  return (
    <Box marginTop={1}>
      {tab === "net"
        ? (
          <NetTab
            status={status}
            policy={policy}
            feed={feed}
            rows={rows}
            scopeLabel={netScopeLabel}
          />
        )
        : tab === "mcp"
        ? <McpTab mcp={mcp} selected={mcpSel} msg={mcpMsg} />
        : tab === "theme"
        ? <ThemeTab state={theme} selected={themeSel} />
        : <SkillsTab skills={skills} rows={rows} />}
    </Box>
  );
}
