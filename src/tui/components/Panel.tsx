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
import { TextAttributes } from "@opentui/core";
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
import { type SkillRow, type SkillSourceRow, SkillsTab } from "./Skills.tsx";
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
  | { type: "confirm" }
  /** ⏎'s sibling: branch AND summarize the abandoned path (`historytree.ts`). */
  | { type: "confirmSummarize" };

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
    case "panel.confirmSummarize":
      return { type: "confirmSummarize" };
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
    case "confirmSummarize":
      // Nothing to preview or revert: the host performs it (`PanelHost`).
      return state;
  }
}

// ---- the panel -------------------------------------------------------------

/**
 * The tab strip.
 *
 * Which tab is open is marked with BRACKETS as well as with weight and hue,
 * because a character is the only encoding that always survives. Colour alone was
 * the whole signal here, so the answer to "which tab am I on" was invisible to a
 * colourblind reader, to a NO_COLOR terminal, and to anything reading the screen
 * as text — which includes every test in this repo that asserts on a rendered row.
 */
export function PanelTabs({ tab, width }: { tab: PanelTab; width?: number }) {
  // A strip that does not fit is worse than no strip: truncation silently drops
  // the tabs at the end, so at 60 columns the theme tab did not appear to exist
  // and neither did the close hint. Below the fitting width it collapses to the
  // open tab, its position, and how to move — every fact the strip carried.
  const full = TABS.reduce((n, t) => n + t.title.length + 2, 0) + 12;
  if (width !== undefined && width < full) {
    const at = TABS.findIndex((t) => t.id === tab) + 1;
    return (
      <text wrapMode="none">
        <span attributes={TextAttributes.DIM}>{" ["}</span>
        <span attributes={TextAttributes.BOLD} fg={palette.accent}>{tab}</span>
        <span attributes={TextAttributes.DIM}>
          {`] ${at}/${TABS.length} · ⇥ next · ^t close`}
        </span>
      </text>
    );
  }
  return (
    <text wrapMode="none">
      {TABS.map((t) => {
        const active = t.id === tab;
        return (
          <span key={t.id}>
            <span attributes={TextAttributes.DIM}>{active ? " [" : "  "}</span>
            <span
              attributes={active ? TextAttributes.BOLD : TextAttributes.NONE}
              fg={active ? palette.accent : undefined}
            >
              {t.title}
            </span>
            <span attributes={TextAttributes.DIM}>{active ? "]" : ""}</span>
          </span>
        );
      })}
      <span attributes={TextAttributes.DIM}>{"   ^t close"}</span>
    </text>
  );
}

export interface PanelProps {
  tab: PanelTab;
  /** Rows the panel body may occupy. */
  rows: number;
  /** Columns available, so the tab strip can collapse instead of truncating. */
  width?: number;
  sessions?: SessionsProps;
  changes?: ChangesProps;
  model?: ModelPickerProps;
  mcp?: McpTabProps;
  skills?: {
    skills: SkillRow[] | null;
    note?: string;
    sources?: readonly SkillSourceRow[];
    selected?: number;
    filter?: string;
    filtering?: boolean;
  };
  theme?: { preview: ThemePreview | null };
  /** Body for the tabs other tasks own (`tree`, `workflows`). */
  children?: ReactNode;
}

/**
 * The blank row between the tab strip and the body — the first thing to go.
 *
 * At eight terminal rows the panel gets ONE content row, and spending it on
 * whitespace pushed the body onto the bottom border: the sessions legend rendered
 * as `╰─↑↓─move─·─pgup/pgdn─page─…─╯`. Breathing room is a luxury and a budget is
 * not; below five rows there is none.
 */
function gapRows(rows: number): number {
  return rows >= 5 ? 1 : 0;
}

/**
 * Rows a tab body may paint — the panel's own budget, minus its own chrome.
 *
 * EXPORTED because `PanelHost` must hand the SAME number to the two tabs whose body
 * arrives as `children` (`tree`, `workflows`). It used to pass them the panel's
 * `rows`, two more than they had, so those two tabs overflowed by two rows every
 * time. One function, so the tab strip's height is subtracted exactly once.
 *
 * The floor is ZERO. `Math.max(3, …)` was the original defect and `Math.max(1, …)`
 * is the same defect one row smaller: a floor is a CLAIM about how much room there
 * is, and a claim that outruns the room is what OpenTUI answers by shrinking rows
 * onto each other. A panel with no room for a body renders no body.
 */
export function panelBodyRows(rows: number): number {
  return Math.max(0, rows - 1 /* the tab strip */ - gapRows(rows));
}

function Body(
  { tab, rows, width, sessions, changes, model, mcp, skills, theme, children }: PanelProps,
) {
  const body = panelBodyRows(rows);
  switch (tab) {
    case "sessions":
      return sessions
        ? <Sessions {...sessions} rows={body} />
        : <text attributes={TextAttributes.DIM}>loading…</text>;
    case "changes":
      return changes
        ? <Changes {...changes} rows={body} />
        : <text attributes={TextAttributes.DIM}>loading…</text>;
    case "model":
      return model
        ? <ModelPicker {...model} rows={body} />
        : <text attributes={TextAttributes.DIM}>loading…</text>;
    case "mcp":
      return <McpTab {...(mcp ?? { status: null, selected: 0 })} rows={body} />;
    case "skills":
      return (
        <SkillsTab
          cols={width}
          selected={skills?.selected}
          skills={skills?.skills ?? null}
          note={skills?.note}
          sources={skills?.sources}
          filter={skills?.filter}
          filtering={skills?.filtering}
          rows={body}
        />
      );
    case "theme":
      return <ThemeTab preview={theme?.preview ?? null} rows={body} />;
    default:
      // `tree` and `workflows`: the slot, or an honest line saying it was not passed.
      return <>{children ?? <text attributes={TextAttributes.DIM}>nothing to show here</text>}</>;
  }
}

export function Panel(props: PanelProps) {
  return (
    // Fixed height, and therefore `flexShrink: 0` — OpenTUI gives an auto-sized
    // renderable `flexShrink: 1`, which is the whole of the corruption below.
    // `props.rows` is the budget for the strip and the body; the border takes two more.
    <box
      flexDirection="column"
      height={props.rows + 2}
      borderStyle="rounded"
      borderColor={palette.border}
      paddingX={1}
    >
      <PanelTabs tab={props.tab} width={props.width} />
      {/*
        TWO BOXES, AND BOTH ARE LOAD-BEARING. This is the fix for the panel resize
        corruption — rows rendering as character-level interleavings of two different
        lines, e.g. `❯ ● ✓ wsvewsor28mGreeting Session  ws  4m`, which was two list
        rows painted onto one screen row.

        The cause was NOT stale cells and NOT React keying: it reproduced on a FRESH
        mount at 100x12. Every `<text>` OpenTUI lays out defaults to `flexShrink: 1`
        (`Renderable`: shrink is 0 only when an explicit width or height is set), so a
        tab body emitting six rows into a three-row box did not overflow — yoga SHRANK
        all six to half a row each, and pairs of them rounded onto the same y and
        overdrew each other. Chat never showed it because Chat renders exactly `body`
        row slots and never one more.

        So: the outer box pins the height and CLIPS (`overflow: hidden` pushes a
        scissor rect), and the inner box carries `flexShrink={0}` so the shrink cannot
        propagate into the tab's own rows. A body that overruns its budget now loses
        its last rows — legible, and the row above it stays a row — instead of
        dissolving. The budgets below are what keep it from overrunning at all; this
        pair is what makes a future tab unable to bring the corruption back.
      */}
      {/*
        A ZERO-ROW BODY IS NOT MOUNTED, and that is not an optimisation. OpenTUI
        pushes a scissor rect only when `width > 0 && height > 0`, so a box of height
        zero clips NOTHING and its children paint wherever yoga puts them — which was
        onto the bottom border: at eight terminal rows the sessions legend rendered as
        `╰─↑↓─move─·─pgup/pgdn─page─…─╯`. No room means no body.
      */}
      {panelBodyRows(props.rows) > 0
        ? (
          <box
            flexDirection="column"
            marginTop={gapRows(props.rows)}
            height={panelBodyRows(props.rows)}
            overflow="hidden"
          >
            <box flexDirection="column" flexShrink={0}>
              <Body {...props} />
            </box>
          </box>
        )
        : null}
    </box>
  );
}
