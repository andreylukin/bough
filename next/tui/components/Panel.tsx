/**
 * ONE tabbed panel. Every non-chat surface is a tab of it.
 *
 * THE INVARIANT THIS HOLDS: **there is exactly one place that is not the chat** (spec
 * §15). The 3,618-line `App.tsx` this rewrite undoes gave each surface its own overlay,
 * its own open flag, its own escape handling and its own opinion about the other seven.
 * Here there is one `PanelState`, one reducer, and a tab that is either showing or not.
 * Adding a surface is adding a row to `TABS`; it cannot add a new way to be open.
 *
 * SECOND — **leaving the theme tab reverts an uncommitted preview** (spec §16: the
 * picker "previews live on cursor move and reverts on exit, so browsing never commits").
 * That is wired *inside* `reducePanel` rather than at each exit key, because there are
 * five ways to leave a tab — ^t, escape, a chord, tab, shift-tab — and a revert
 * remembered at four of them is a TUI that silently keeps the theme you last scrolled
 * past. `cancel()` is idempotent, so the reducer calls it on every departure from
 * `theme` and on nothing else.
 *
 * THIRD — **the keymap is data.** `TABS` carries each tab's chord, so "no duplicate
 * bindings" is a check over an array rather than a reading of a switch. The chords are
 * read in raw mode, where the line discipline is off (^S is not XOFF, ^D is not EOF).
 * They overlap the composer's readline chords (^d, ^k, ^w), which `tui/keys.ts` already
 * resolves with an `emptyDraft` guard on its own `view.tree`/`view.workflows` chords:
 * the composition root offers a key to the composer first and reaches `panelAction` only
 * with an empty draft. `panelAction` does not read the draft — that is not panel state.
 *
 * OWNERSHIP. The `mcp`, `skills` and `theme` bodies are small and live below;
 * `sessions`, `changes` and `model` are their own files. `tree` and `workflows` belong
 * to other tasks and arrive as `children`, so their prop shapes stay theirs and this
 * file stays chrome.
 *
 * Ported from `src/tui/components/Panel.tsx` (tab list, preview-on-cursor theme tab);
 * the state machine and the keymap-as-data are new.
 */
import { Box, type Key, Text } from "ink";
import type { ReactNode } from "react";
import type { McpStatus } from "../../mcp/status.ts";
import { clip, windowAround } from "../format.ts";
import { palette, presetSwatch, type ThemePreview } from "../theme.ts";
import { Changes, type ChangesProps } from "./Changes.tsx";
import { ModelPicker, type ModelPickerProps } from "./ModelPicker.tsx";
import { Sessions, type SessionsProps } from "./Sessions.tsx";

// ---- the tabs (data) -------------------------------------------------------

/** `^t` toggles the panel; every `chord` below jumps straight to its tab. */
export const TOGGLE_CHORD = "t";

export const TABS = [
  { id: "sessions", title: "sessions", chord: "s" },
  // ^f and ^w match the `view.tree` / `view.workflows` chords `tui/keys.ts` already
  // binds, so the two tables name the same surface with the same key.
  { id: "tree", title: "tree", chord: "f" },
  { id: "changes", title: "changes", chord: "d" },
  { id: "workflows", title: "workflows", chord: "w" },
  { id: "model", title: "model", chord: "o" },
  { id: "mcp", title: "mcp", chord: "p" },
  { id: "skills", title: "skills", chord: "k" },
  { id: "theme", title: "theme", chord: "y" },
] as const satisfies readonly { id: string; title: string; chord: string }[];

/** Derived from `TABS`, so the tab set and the keymap cannot disagree. */
export type PanelTab = (typeof TABS)[number]["id"];
export type TabDef = (typeof TABS)[number];

export const PANEL_TABS: readonly PanelTab[] = TABS.map((t) => t.id);

/** The tab a chord opens, or `null`. `^t` is the toggle and never names a tab. */
export function tabForChord(chord: string): PanelTab | null {
  return TABS.find((t) => t.chord === chord)?.id ?? null;
}

// ---- the state machine (pure, but for the theme preview it must cancel) ----

export interface PanelState {
  open: boolean;
  tab: PanelTab;
}

export const initialPanel: PanelState = { open: false, tab: "sessions" };

export type PanelAction =
  | { type: "toggle" }
  | { type: "close" }
  | { type: "jump"; tab: PanelTab }
  /** Tab / shift-tab through the bar. */
  | { type: "cycle"; delta: number }
  /** Cursor movement inside the active tab. */
  | { type: "move"; delta: number }
  | { type: "confirm" };

/** `Partial` of ink's own `Key`, so a test builds one as a literal, with no terminal. */
export type PanelKey = Partial<Key>;

/**
 * Keypress → action, or `null` for "not mine, pass it on". The panel claims arrows,
 * enter and escape only while it is open — a closed panel must not eat the composer's
 * keys, and a chord must work from anywhere or it is not a direct jump.
 */
export function panelAction(input: string, key: PanelKey, open: boolean): PanelAction | null {
  if (key.ctrl) {
    const letter = input.toLowerCase();
    if (letter === TOGGLE_CHORD) return { type: "toggle" };
    const tab = tabForChord(letter);
    return tab ? { type: "jump", tab } : null;
  }
  if (!open) return null;
  if (key.escape) return { type: "close" };
  if (key.tab) return { type: "cycle", delta: key.shift ? -1 : 1 };
  if (key.upArrow) return { type: "move", delta: -1 };
  if (key.downArrow) return { type: "move", delta: 1 };
  if (key.return) return { type: "confirm" };
  return null;
}

export interface PanelDeps {
  /**
   * The live theme preview, when the theme tab is in use. The reducer drives it:
   * cursor movement previews, enter keeps, and LEAVING THE TAB REVERTS.
   */
  theme?: Pick<ThemePreview, "move" | "commit" | "cancel">;
}

/**
 * Apply an action. The only side effects are on the injected theme preview — the panel
 * owns "browsing never commits" because it is the thing that knows you left.
 */
export function reducePanel(
  state: PanelState,
  action: PanelAction,
  deps: PanelDeps = {},
): PanelState {
  const leave = (next: PanelState): PanelState => {
    const leaving = state.open && (!next.open || next.tab !== state.tab);
    if (leaving && state.tab === "theme") deps.theme?.cancel();
    return next;
  };
  switch (action.type) {
    case "toggle":
      return leave({ ...state, open: !state.open });
    case "close":
      return leave({ ...state, open: false });
    case "jump":
      // The chord that brought you here takes you back: jumping to the open tab closes.
      return leave(
        state.open && state.tab === action.tab
          ? { ...state, open: false }
          : { open: true, tab: action.tab },
      );
    case "cycle": {
      const at = PANEL_TABS.indexOf(state.tab);
      const next = PANEL_TABS[(at + action.delta + PANEL_TABS.length) % PANEL_TABS.length];
      return leave({ open: true, tab: next });
    }
    case "move":
      if (state.open && state.tab === "theme") deps.theme?.move(action.delta);
      return state;
    case "confirm":
      if (state.open && state.tab === "theme") deps.theme?.commit();
      return state;
  }
}

// ---- tab bodies owned here -------------------------------------------------

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
      <Text dimColor wrap="truncate">↑↓ move · ⏎ grant/revoke · a authorize · r restart</Text>
    </Box>
  );
}

/**
 * One installed skill. Declared here rather than imported: skill discovery is T10.2 and
 * has no module yet, so there is no wire shape to import — only these two fields are
 * ever shown, and the frontmatter that carries them is fixed by spec §16.
 */
export interface SkillRow {
  name: string;
  description: string;
}

export function SkillsTab({ skills, rows }: { skills: SkillRow[] | null; rows: number }) {
  if (!skills) return <Text dimColor>loading…</Text>;
  if (skills.length === 0) return <Text dimColor>no skills installed</Text>;
  const height = Math.max(3, rows - 6);
  return (
    <Box flexDirection="column">
      {skills.slice(0, height).map((s) => (
        <Text key={s.name} wrap="truncate">
          <Text bold color={palette.accent}>/{s.name}</Text>
          <Text dimColor>{"  "}{clip(s.description, 60)}</Text>
        </Text>
      ))}
      {skills.length > height ? <Text dimColor>… {skills.length - height} more</Text> : null}
      <Text dimColor wrap="truncate">name a skill in your message to load it</Text>
    </Box>
  );
}

export function ThemeTab({ preview, rows }: { preview: ThemePreview | null; rows: number }) {
  if (!preview) return <Text dimColor>loading theme…</Text>;
  const height = Math.max(3, rows - 5);
  const { start, end } = windowAround(preview.index, preview.presets.length, height);
  return (
    <Box flexDirection="column">
      <Text dimColor wrap="truncate">
        {preview.previewing ? "previewing " : "current: "}
        {preview.name} — ↑↓ preview live · ⏎ keep · leaving the tab reverts
      </Text>
      {preview.presets.slice(Math.max(0, start), end).map((p, i) => {
        const sel = Math.max(0, start) + i === preview.index;
        return (
          <Box key={p.name}>
            <Text wrap="truncate">
              <Text color={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</Text>
              <Text bold={sel}>{p.name.padEnd(16)}</Text>
            </Text>
            <Text wrap="truncate">
              {presetSwatch(p).map((cell) => (
                <Text key={cell.token} color={cell.color}>{cell.block}</Text>
              ))}
            </Text>
            <Text dimColor wrap="truncate">{"  "}{p.note}</Text>
          </Box>
        );
      })}
    </Box>
  );
}

// ---- the panel -------------------------------------------------------------

export function PanelTabs({ tab }: { tab: PanelTab }) {
  return (
    <Text wrap="truncate">
      {TABS.map((t) => (
        <Text key={t.id}>
          <Text dimColor>{"  "}</Text>
          <Text bold={t.id === tab} color={t.id === tab ? palette.accent : undefined}>
            {t.title}
          </Text>
        </Text>
      ))}
      <Text dimColor>{"   ^t close"}</Text>
    </Text>
  );
}

export interface PanelProps {
  tab: PanelTab;
  /** Rows the panel body may occupy. */
  rows: number;
  sessions?: SessionsProps;
  changes?: ChangesProps;
  model?: ModelPickerProps;
  mcp?: McpTabProps;
  skills?: { skills: SkillRow[] | null };
  theme?: { preview: ThemePreview | null };
  /** Body for the tabs other tasks own (`tree`, `workflows`). */
  children?: ReactNode;
}

function Body({ tab, rows, sessions, changes, model, mcp, skills, theme, children }: PanelProps) {
  const body = Math.max(3, rows - 2);
  switch (tab) {
    case "sessions":
      return sessions ? <Sessions {...sessions} rows={body} /> : <Text dimColor>loading…</Text>;
    case "changes":
      return changes ? <Changes {...changes} rows={body} /> : <Text dimColor>loading…</Text>;
    case "model":
      return model ? <ModelPicker {...model} rows={body} /> : <Text dimColor>loading…</Text>;
    case "mcp":
      return <McpTab {...(mcp ?? { status: null, selected: 0 })} />;
    case "skills":
      return <SkillsTab skills={skills?.skills ?? null} rows={body} />;
    case "theme":
      return <ThemeTab preview={theme?.preview ?? null} rows={body} />;
    default:
      // `tree` and `workflows`: the slot, or an honest line saying it was not passed.
      return <>{children ?? <Text dimColor>nothing to show here</Text>}</>;
  }
}

export function Panel(props: PanelProps) {
  return (
    <Box flexDirection="column" borderStyle="round" borderColor={palette.border} paddingX={1}>
      <PanelTabs tab={props.tab} />
      <Box flexDirection="column" marginTop={1}>
        <Body {...props} />
      </Box>
    </Box>
  );
}
