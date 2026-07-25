// The tabbed side-panel: mcp (server registry), skills. App owns the data and key
// handling.
import { palette, THEME_PRESETS } from "../theme.ts";
import { Box } from "ink";
import { Text } from "./Text.tsx";
import type { McpStatus, SkillInfo, ThemeState } from "../api.ts";
import { SelFill, SelRow } from "./SelRow.tsx";

// The management view is ONE tabbed panel — every non-chat surface is a tab:
// session tree, conversation branch tree, changes review, model/keys, and the
// mcp/skills/theme tabs. ^t toggles it; ^p/^f/^d/^o jump to a tab.
export type PanelTab =
  | "sessions"
  | "conversation"
  | "changes"
  | "workflows"
  | "jobs"
  | "model"
  | "mcp"
  | "skills"
  | "theme";
export const PANEL_TABS: PanelTab[] = [
  "sessions",
  "conversation",
  "changes",
  "workflows",
  "jobs",
  "model",
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
        const sel = i === selected;
        return (
          // The status glyph drops its color under selection: an inverse colored
          // fg reads as a colored bg speck inside the light bar.
          <SelRow key={name} sel={sel}>
            <Text
              color={sel
                ? undefined
                : conn?.alive
                ? palette.accent
                : active
                ? palette.warn
                : undefined}
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
          </SelRow>
        );
      })}
      {msg ? <Text color={palette.warn} wrap="wrap">{msg}</Text> : null}
    </Box>
  );
}

/** Swatch tokens shown per preset row, in order: surfaces, accent, text.
 * Surfaces get the wider cells — near-identical dark presets (Default vs
 * Midnight) differ only there, so they need the extra area to be tellable. */
const SWATCH_TOKENS = ["bg", "panel", "panelInset", "green", "text"];
const SURFACE_TOKENS = new Set(["bg", "panel", "panelInset"]);

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
        const sel = i === selected;
        // The swatch strip stays OUTSIDE the inverse run: inverse would swap the
        // cell colors into the background and flatten the very preview the
        // selected row is applying. Hairline separators keep adjacent
        // near-identical surfaces tellable apart.
        return (
          <Box key={p.name}>
            <Text inverse={sel} wrap="truncate">
              <Text color={sel ? undefined : palette.accent}>{active ? "●" : " "}</Text>{" "}
              {p.name.padEnd(15)}
              {" "}
            </Text>
            <Text wrap="truncate">
              {SWATCH_TOKENS.map((t, j) => (
                <Text key={t}>
                  {j > 0 ? <Text color={palette.border}>▏</Text> : null}
                  <Text color={row[t]}>{SURFACE_TOKENS.has(t) ? "███" : "██"}</Text>
                </Text>
              ))}
            </Text>
            <Text inverse={sel} wrap="truncate">
              <Text dimColor>{"  "}{p.note}</Text>
            </Text>
            <SelFill sel={sel} />
          </Box>
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
  { tab, mcp, mcpSel, mcpMsg, skills, rows, theme, themeSel }: {
    tab: PanelTab;
    mcp: McpStatus | null;
    mcpSel: number;
    mcpMsg: string | null;
    skills: SkillInfo[] | null;
    rows: number;
    theme: ThemeState | null;
    themeSel: number;
  },
) {
  // Content only — the unified panel container owns the border + tab bar.
  // Column direction so the active tab stretches to the panel's full width —
  // in a row box the tab sizes to content and selection bars stop short.
  return (
    <Box marginTop={1} flexDirection="column">
      {tab === "mcp"
        ? <McpTab mcp={mcp} selected={mcpSel} msg={mcpMsg} />
        : tab === "theme"
        ? <ThemeTab state={theme} selected={themeSel} />
        : <SkillsTab skills={skills} rows={rows} />}
    </Box>
  );
}
