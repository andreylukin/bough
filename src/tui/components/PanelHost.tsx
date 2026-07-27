/**
 * The panel's controller: cursor, fetches, and what ⏎ means on each tab.
 *
 * THE INVARIANT THIS HOLDS: **`App.tsx` does not grow when a tab is added.** The
 * composition root's whole contribution to the panel is one hook call and one
 * `if (panel.handle(command)) return;`. Everything a tab needs — which row the cursor
 * is on, when to re-fetch, what its affirmative does — is here, so the file that must
 * stay small stays small. The old tree's 3,618-line `App.tsx` is what happens when
 * this file does not exist.
 *
 * SECOND — **one cursor, reset on every tab change.** Eight tabs sharing one index and
 * clearing it on arrival is deliberate: per-tab cursor memory is state that outlives
 * the data it points into (a session list that shrank, a change set that was reverted),
 * and a cursor pointing at row 9 of a three-row tab is the class of bug that made the
 * old overlays feel broken. Arriving at the top is always correct.
 *
 * THIRD — **MCP state is re-fetched on every entry and never cached** (plan §6.13):
 * grants and connections change between turns, so a panel painted from last minute's
 * status would disagree with the model's own `mcpStatus()` call. Changes and workflows
 * refresh on entry for the weaker version of the same reason.
 *
 * FOURTH — **absent capability is stated, never faked.** One thing the panel shows
 * still has no server route: persisting a model choice (there is no
 * `PATCH /sessions/:id`; a model is set when a session is created). It renders the
 * sentence saying so. It does not sit on "loading…", which is a hang wearing a
 * spinner, and it does not report a success it did not have. Skills and theme USED to
 * be in this list and are not any more — both routes landed, and the fix for a closed
 * gap is to wire it, not to keep printing the apology.
 *
 * NO I/O OF ITS OWN. Every fetch is an injected thunk supplied by `tui/main.tsx` or a
 * method on the store. This hook builds no client and knows no URL, which is what lets
 * `App.test.tsx` drive the whole panel from fixtures with no server.
 */
import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import type { McpStatus } from "../../mcp/status.ts";
import type { ModelRow } from "../../llm/client.ts";
import { api, type SessionRow } from "../api.ts";
import type { Command, PanelTab } from "../keys.ts";
import type { Store, TuiState } from "../store.ts";
import {
  createThemePreview,
  type ThemePreset,
  type ThemePreview,
  type ThemeState,
} from "../theme.ts";
import { initialPanel, Panel, panelActionFor, type PanelState, reducePanel } from "./Panel.tsx";
import { changeItems } from "./Changes.tsx";
import { sessionItems } from "./Sessions.tsx";
import { chooseEntry, type ModelConfig, modelEntries } from "./ModelPicker.tsx";
import type { SkillRow, SkillSourceRow } from "./Skills.tsx";
import { Tree, type TreeItem } from "./Tree.tsx";
import { Workflows } from "./Workflows.tsx";

/**
 * Why the list is absent when it is. Only reachable now if the composition root
 * declined to inject the fetch or the fetch itself failed — never a claim about what
 * the user has installed, which is the distinction `Skills.tsx` exists to keep.
 */
const SKILLS_NOTE =
  "the skills list could not be read from this server — GET /skills did not answer, " +
  "so this is not a claim that you have none installed";

/** What a model choice cannot do yet, said at the moment someone tries it. */
const MODEL_NOTE = "pinned in this client only — there is no route to persist a model " +
  "(no PATCH /sessions/:id); a model is set when a session is created";

/**
 * The changes tab with no conversation open.
 *
 * `store.refreshChanges()` returns without fetching when there is no session, which
 * leaves `state.changes` at `null` — and `null` means "the fetch is in flight". Left
 * alone, the tab sits on "loading changes…" forever at cold start, which is a hang
 * wearing a spinner. Spec §13's distinction between "no change set" and "nothing
 * changed" applies to this case too, so it gets its own sentence.
 *
 * …and the non-git HINT is suppressed for it (`hint: null` below). `Changes.tsx`
 * prints "the agent still works here — this checkout produces nothing reviewable"
 * under an unavailable change set, which is exactly right for the case spec §13 names
 * and false for this one: there is no checkout, because there is no session. Two
 * different absences, two different sentences.
 */
const NO_SESSION_CHANGES = {
  available: false as const,
  reason: "no conversation is open — open or start one and its changes appear here",
  base: null,
  files: [],
  workspace: "",
};

/**
 * The operations the store does not expose. Every one is a REST call `tui/api.ts`
 * already has a method for; they are injected so this module keeps its no-I/O
 * property and a test drives them with three lines of fakes.
 */
export interface PanelControls {
  /** `GET /sessions?originId=` — delegated children, for the tree and the rail. */
  listChildren?: (originId: string) => Promise<SessionRow[]>;
  /** `GET /mcp/servers?session=` — re-read on every entry, never cached. */
  loadMcp?: (sessionId?: string) => Promise<McpStatus>;
  /** `POST /mcp/servers/:name/{enable,disable}` — the grant itself. */
  setMcpEnabled?: (name: string, on: boolean, sessionId: string) => Promise<unknown>;
  /**
   * `GET /skills` — a fresh walk of the source directories, so a skill written a
   * second ago lists a second later. `sources` rides along because an empty list
   * cannot otherwise be told apart from reading the wrong directory.
   */
  loadSkills?: () => Promise<{ skills: SkillRow[]; sources: SkillSourceRow[] }>;
  pauseWorkflow?: (id: string) => Promise<void>;
  resumeWorkflow?: (id: string) => Promise<void>;
  stopWorkflow?: (id: string) => Promise<void>;
  rerunWorkflow?: (id: string) => Promise<void>;
}

export interface PanelHostDeps {
  store: Store;
  state: TuiState;
  rows: number;
  cols: number;
  now: number;
  controls?: PanelControls;
  /**
   * The picker's catalog. A prop and not an import: `llm/client.ts` pulls the
   * provider SDK, and a component that imported it would drag the whole model layer
   * into the TUI process. `tui/main.tsx` passes it; a test passes three rows.
   */
  models?: readonly ModelRow[];
  /**
   * The theme's two server-facing halves (spec §16: a theme is persisted server-side
   * and the TUI fetches it at boot). Absent = the pre-T10.4 behaviour, a picker whose
   * choice lasts for this process only — which is what a fixture-driven test wants.
   */
  theme?: {
    /** What `GET /theme` served at boot. The baseline `cancel()` restores. */
    current?: ThemeState | null;
    /** `PUT`/`DELETE /theme`. Fire-and-forget: a failed save must not unpaint. */
    persist?: (preset: ThemePreset, state: ThemeState) => unknown;
  };
  /** The tree tab's rows, and the drill-in `App` already owns for the rail. */
  tree: TreeItem[];
  drillIn: (originId: string) => void;
  collapse: (originId: string) => void;
}

export interface PanelHandle {
  open: boolean;
  tab: PanelTab;
  /** True when the command was the panel's. `App` returns immediately on true. */
  handle: (command: Command) => boolean;
  /** The mounted panel, or `null` when it is closed. */
  view: ReactNode;
}

export function usePanelHost(deps: PanelHostDeps): PanelHandle {
  const { store, state, rows, cols, now, controls = {}, models = [] } = deps;
  const { tree, drillIn, collapse } = deps;
  const [panel, setPanel] = useState<PanelState>(initialPanel);
  const [sel, setSel] = useState(0);
  const [focusDiff, setFocusDiff] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [skills, setSkills] = useState<SkillRow[] | null>(null);
  const [skillSources, setSkillSources] = useState<SkillSourceRow[]>([]);
  // Tri-state, because `skills === null` alone cannot tell "the fetch is in flight"
  // from "the fetch failed", and those are a spinner and a sentence respectively.
  const [loadingSkills, setLoadingSkills] = useState(false);
  const [cfg, setCfg] = useState<ModelConfig>({
    defaultModel: "",
    sessionModel: null,
    cheapModel: null,
    defaultEffort: "default",
    sessionEffort: null,
  });

  // The theme preview is one object per TUI session, not per entry into the tab: its
  // baseline is what a commit moves, and rebuilding it on every visit would make each
  // visit's "revert" restore the previous visit's preview instead of the real theme.
  //
  // `deps.theme` carries the two halves the preview cannot supply itself: the state
  // the server served at boot (the baseline `cancel()` restores — without it every
  // session starts from "Default" and leaving the tab REVERTS a stored theme off the
  // screen) and the writer `commit()` calls (without it keeping a theme lasts until
  // the process exits, which is a picker that silently forgets). Both are injected so
  // this hook keeps its no-I/O property; `tui/main.tsx` supplies them.
  const [preview] = useState<ThemePreview>(() =>
    createThemePreview({
      ...(deps.theme?.current !== undefined ? { current: deps.theme.current } : {}),
      ...(deps.theme?.persist ? { persist: deps.theme.persist } : {}),
    })
  );

  const sessions = useMemo(() => sessionItems(state.sessions), [state.sessions]);
  const changes = useMemo(() => changeItems(state.changes), [state.changes]);
  const entries = useMemo(() => modelEntries(models), [models]);

  // The picker reads the open session's pin; `chooseEntry` writes into a local copy.
  const modelCfg: ModelConfig = {
    ...cfg,
    // `state.effectiveModel` is the server's answer to "what will the next turn
    // actually call", resolved exactly the way the runner resolves it. It comes
    // BEFORE the catalog fallback, which was a guess wearing the ● that means
    // fact: with no stored config and no session pin, the picker marked the first
    // row of the catalog while the meter — correctly — named the model from the
    // environment. Two surfaces of the same app disagreed about what was running.
    defaultModel: cfg.defaultModel || state.session?.model || state.effectiveModel ||
      models[0]?.id || "(unset)",
    sessionModel: cfg.sessionModel ?? state.session?.model ?? null,
  };

  // Entering a tab is what refreshes it. Grants and connections change between turns,
  // so MCP is re-read every time rather than remembered (plan §6.13).
  useEffect(() => {
    if (!panel.open) return;
    // The server owns the answer to "what will a new conversation run on"; the
    // picker used to guess it from the catalog. Fetched on entry, like every other
    // tab, and only when it is not already known.
    if (panel.tab === "model" && !cfg.defaultModel) {
      void api.getModelSettings()
        .then((s) => setCfg((c) => (c.defaultModel ? c : { ...c, defaultModel: s.defaultModel })))
        .catch(() => {}); // an unreachable server already shows as disconnected
    }
    if (panel.tab === "changes") void store.refreshChanges();
    if (panel.tab === "workflows") void store.refreshWorkflows();
    if (panel.tab === "mcp") {
      setMcp(null);
      void controls.loadMcp?.(state.currentId ?? undefined)
        .then(setMcp)
        .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
    }
    if (panel.tab === "skills") {
      // Re-read on entry, like MCP and for a weaker version of the same reason: a
      // skill is a folder on disk that the user (or the agent) may have written since
      // the panel was last open, and nothing here caches (`server/skills.ts`).
      if (controls.loadSkills) {
        setLoadingSkills(true);
        void controls.loadSkills()
          .then((r) => {
            setSkills(r.skills);
            setSkillSources(r.sources);
          })
          // `null` and not `[]`: a failed fetch must not read as "none installed".
          .catch(() => setSkills(null))
          .finally(() => setLoadingSkills(false));
      }
    }
    // Deliberately NOT `controls`: the object is rebuilt on every render of the
    // composition root, and an effect that depends on it re-runs forever. The two
    // thunks it actually reads are stable, built once in `tui/main.tsx`.
  }, [
    panel.open,
    panel.tab,
    state.currentId,
    store,
    controls.loadMcp,
    controls.loadSkills,
    cfg.defaultModel,
  ]);

  const items = tabLength(panel.tab, {
    sessions: sessions.length,
    tree: tree.length,
    changes: changes.length,
    workflows: state.workflows.length,
    model: entries.length,
    mcp: mcp ? Object.keys(mcp.registry.servers).length : 0,
    skills: skills?.length ?? 0,
    // The theme tab paints from `preview.index`, not from `sel` — but the preview is a
    // MUTABLE object outside React, so moving it changes no state and schedules no
    // render. Giving the tab a real row count makes `sel` move with it, and that is
    // what repaints. Without it the palette recoloured while the cursor stayed on row
    // one: caught by driving the real TUI, not by any test that existed.
    theme: preview.presets.length,
  });

  /**
   * Reduce against the CURRENT state and set the result, rather than passing an
   * updater to `setPanel`. Two reasons, both learned the hard way here: `reducePanel`
   * has a side effect (it reverts an uncommitted theme preview), and an updater runs
   * in the render phase where React may call it twice and where calling another
   * component's setter is undefined behaviour. A keypress is an event, it happens
   * once, and the state it reduces from is the one on screen.
   */
  const dispatch = useCallback((action: Parameters<typeof reducePanel>[1]) => {
    setPanel(reducePanel(panel, action, { theme: preview }));
  }, [panel, preview]);

  // Arriving at a tab arrives at its top, with no message and no held diff focus.
  useEffect(() => {
    setSel(0);
    setMessage(null);
    setFocusDiff(false);
  }, [panel.tab, panel.open]);

  /** ⏎ on the active tab. One place, so a tab's affirmative is one line of code. */
  const confirm = useCallback(() => {
    switch (panel.tab) {
      case "sessions": {
        const item = sessions[sel];
        if (!item) return;
        dispatch({ type: "close" });
        return void store.open(item.session.id);
      }
      case "tree": {
        const item = tree[sel];
        if (!item || item.type !== "session") return;
        dispatch({ type: "close" });
        return void store.open(item.session.id);
      }
      case "changes":
        // NOT revert: revert deletes untracked files and restores tracked ones, and ⏎
        // is the key a cursor lands on. This gives the diff the whole tab instead.
        return setFocusDiff((v) => !v);
      case "workflows": {
        // Spec §8: replay is ALWAYS reported. This is the client half of that.
        const run = state.workflows[sel];
        return run ? void store.refreshReplay(run.id) : undefined;
      }
      case "model": {
        const entry = entries[sel];
        if (!entry) return;
        setCfg(chooseEntry(modelCfg, entry));
        return setMessage(MODEL_NOTE);
      }
      case "mcp": {
        const name = mcp ? Object.keys(mcp.registry.servers).sort()[sel] : undefined;
        if (!name || !mcp) return;
        const on = !mcp.active.includes(name);
        if (!controls.setMcpEnabled) {
          return setMessage("granting an MCP server is not wired into this client yet");
        }
        return void controls.setMcpEnabled(name, on, state.currentId ?? "")
          .then(() => controls.loadMcp?.(state.currentId ?? undefined).then(setMcp))
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
      }
      default:
        // `theme` commits inside the reducer; `skills` has nothing to affirm.
        return;
    }
  }, [
    dispatch,
    panel.tab,
    sel,
    sessions,
    tree,
    state.workflows,
    entries,
    modelCfg,
    mcp,
    controls,
    store,
  ]);

  const steer = useCallback((run: ((id: string) => Promise<void>) | undefined, verb: string) => {
    const target = state.workflows[sel];
    if (!target) return;
    if (!run) return setMessage(`${verb} is not wired up in this client yet`);
    void run(target.id)
      .then(() => store.refreshWorkflows())
      .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
  }, [state.workflows, sel, store]);

  const handle = useCallback((command: Command): boolean => {
    const action = panelActionFor(command);
    if (action) {
      // A chord opens the panel from outside it; everything else needs it open.
      const fromOutside = action.type === "toggle" || action.type === "jump";
      if (!panel.open && !fromOutside) return false;
      if (action.type === "move") {
        setSel((i) => Math.max(0, Math.min(Math.max(0, items - 1), i + action.delta)));
      }
      dispatch(action);
      if (action.type === "confirm") confirm();
      return true;
    }
    if (!panel.open) return false;
    switch (command) {
      case "move.in": {
        const item = tree[sel];
        if (panel.tab !== "tree" || !item) return true;
        drillIn(item.type === "session" ? item.session.id : item.originId);
        return true;
      }
      case "move.out": {
        const item = tree[sel];
        if (panel.tab !== "tree" || !item) return true;
        collapse(item.type === "session" ? item.session.id : item.originId);
        return true;
      }
      case "wf.pause":
        steer(controls.pauseWorkflow, "pause");
        return true;
      case "wf.resume":
        steer(controls.resumeWorkflow, "resume");
        return true;
      case "wf.stop":
        steer(controls.stopWorkflow, "stop");
        return true;
      case "wf.rerun":
        steer(controls.rerunWorkflow, "relaunch");
        return true;
      default:
        return false;
    }
  }, [
    dispatch,
    panel.open,
    panel.tab,
    items,
    sel,
    tree,
    drillIn,
    collapse,
    steer,
    controls,
    confirm,
  ]);

  const body = Math.max(4, rows - 4);
  const view = panel.open
    ? (
      <Panel
        tab={panel.tab}
        rows={body}
        width={cols}
        sessions={{
          items: sessions,
          selected: sel,
          currentId: state.currentId,
          rows: body,
          now,
          message,
        }}
        changes={{
          set: state.currentId ? state.changes : NO_SESSION_CHANGES,
          ...(state.currentId ? {} : { hint: null }),
          items: changes,
          selected: sel,
          rows: body,
          focused: focusDiff,
          message,
        }}
        model={{ cfg: modelCfg, entries, selected: sel, rows: body, message }}
        mcp={{ status: mcp, selected: sel, message }}
        skills={{
          skills,
          sources: skillSources,
          note: skills === null && !loadingSkills ? SKILLS_NOTE : undefined,
        }}
        theme={{ preview }}
      >
        {panel.tab === "tree" ? <Tree items={tree} selected={sel} rows={body} /> : (
          <Workflows
            runs={state.workflows}
            sel={sel}
            level={0}
            detail={null}
            phaseSel={0}
            agentSel={0}
            scroll={0}
            filter={null}
            promptOpen={false}
            rows={body}
            cols={cols}
            now={now}
          />
        )}
      </Panel>
    )
    : null;

  return { open: panel.open, tab: panel.tab, handle, view };
}

/** How many rows the cursor may walk on this tab. Kept out of the hook body. */
function tabLength(tab: PanelTab, counts: Record<PanelTab, number>): number {
  return counts[tab];
}
