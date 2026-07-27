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
 * THIRD — **the keymap is data, and it is not this file's data.** `TABS` lives in
 * `tui/keys.ts` and is imported here, so a tab and its direct-jump chord are declared
 * once, in the module that resolves keypresses. This file re-exports the table for the
 * components and tests that read it as panel vocabulary, and `panelActionFor` is the
 * only translation between the two: `Command` in, `PanelAction` out. There is no
 * second chord table and no second place a key is interpreted.
 *
 * OWNERSHIP. Every tab body is its own file — `Sessions`, `Changes`, `ModelPicker`,
 * `McpTab`, `SkillsTab`, `ThemeTab` — and `tree` and `workflows` arrive as `children`,
 * so their prop shapes stay theirs and this file stays chrome. `PanelHost.tsx` holds
 * the cursor, the fetches and the confirm dispatch; this is the presentation.
 *
 * Ported from `src/tui/components/Panel.tsx` (tab list, preview-on-cursor theme tab);
 * the state machine and the keymap-as-data are new.
 */
import { Box, Text } from "ink";
import type { ReactNode } from "react";
import {
  type Command,
  PANEL_TABS,
  type PanelTab,
  type TabDef,
  tabForChord,
  tabForCommand,
  TABS,
} from "../keys.ts";
import type { ThemePreview } from "../theme.ts";
import { palette } from "../theme.ts";
import { Changes, type ChangesProps } from "./Changes.tsx";
import { ModelPicker, type ModelPickerProps } from "./ModelPicker.tsx";
import { Sessions, type SessionsProps } from "./Sessions.tsx";
import { McpTab, type McpTabProps } from "./Mcp.tsx";
import { type SkillRow, SkillsTab, type SkillSourceRow } from "./Skills.tsx";
import { ThemeTab } from "./Theme.tsx";

// The panel's vocabulary, re-exported so a reader of this file never has to know it
// is declared in the keymap. The declaration is there; the meaning is here.
export { McpTab, ModelPicker, PANEL_TABS, SkillsTab, tabForChord, TABS, ThemeTab };
export type { McpTabProps, PanelTab, SkillRow, SkillSourceRow, TabDef };

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

/**
 * A resolved keymap command → a panel action, or `null` for "not mine".
 *
 * This is the whole seam between `keys.ts` and the panel. It reads a `Command` and
 * never a keypress, so which key produced it — and whether the composer had first
 * refusal — is settled before anything here runs.
 */
export function panelActionFor(command: Command): PanelAction | null {
  const tab = tabForCommand(command);
  if (tab) return { type: "jump", tab };
  switch (command) {
    case "panel.toggle":
      return { type: "toggle" };
    case "panel.close":
      return { type: "close" };
    case "panel.next":
      return { type: "cycle", delta: 1 };
    case "panel.prev":
      return { type: "cycle", delta: -1 };
    case "panel.confirm":
      return { type: "confirm" };
    case "move.up":
      return { type: "move", delta: -1 };
    case "move.down":
      return { type: "move", delta: 1 };
    default:
      return null;
  }
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
  skills?: { skills: SkillRow[] | null; note?: string; sources?: readonly SkillSourceRow[] };
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
      return (
        <SkillsTab
          skills={skills?.skills ?? null}
          note={skills?.note}
          sources={skills?.sources}
          rows={body}
        />
      );
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
