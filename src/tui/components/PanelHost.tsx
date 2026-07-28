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
 * FOURTH — **absent capability is stated, never faked; and a CLOSED gap gets wired,
 * not re-apologised for.** Nothing the panel shows is missing a route any more. The
 * model tab was the last holdout and its apology had gone stale: it printed "there is
 * no route to persist a model (no `PATCH /sessions/:id`)" long after that route
 * landed (`server/app.ts`), so the picker pinned a model in this client only and told
 * the user to stop expecting more. It writes through `store.setModel` now. Skills and
 * theme went the same way before it. What survives of the rule is the other half: a
 * tab does not sit on "loading…", which is a hang wearing a spinner, and it never
 * reports a success it did not have.
 *
 * FIFTH — **a row's budget is the host's business, and it is exact.** `bodyRows` is
 * what a tab body may paint, and the same number is handed to the tab bodies `Panel`
 * mounts, to the two that arrive as `children`, and to the digit resolver. Each list
 * tab exports the window function it renders from and this file calls that same
 * function, so "which rows are on screen" has one answer. Two answers is how `1`–`9`
 * would come to select a row nobody can see — and how the panel came to paint six
 * rows into three (`Panel.tsx`).
 *
 * NO I/O OF ITS OWN. Every fetch is an injected thunk supplied by `tui/main.tsx` or a
 * method on the store. This hook builds no client and knows no URL, which is what lets
 * `App.test.tsx` drive the whole panel from fixtures with no server.
 */
import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import type { McpStatus } from "../../mcp/status.ts";
import type { ModelRow } from "../../llm/client.ts";
import { api, type SessionRow, type WorkflowDetail } from "../api.ts";
import { selectionFor, type TreeRow } from "../historytree.ts";
import { ConversationTree } from "./History.tsx";
import type { Command, PanelTab } from "../keys.ts";
import type { Store, TuiState } from "../store.ts";
import {
  createThemePreview,
  palette,
  type ThemePreset,
  type ThemePreview,
  type ThemeState,
} from "../theme.ts";
import {
  initialPanel,
  Panel,
  panelActionFor,
  panelBodyRows,
  type PanelState,
  reducePanel,
} from "./Panel.tsx";
import { changeItems, type PendingRevert } from "./Changes.tsx";
import { sessionItems, sessionsWindow } from "./Sessions.tsx";
import {
  asEffortChoice,
  chooseEntry,
  displayRows,
  type ModelConfig,
  modelEntries,
  modelWindow,
  visibleEntries,
} from "./ModelPicker.tsx";
import { type SkillRow, type SkillSourceRow, skillsWindow } from "./Skills.tsx";
import { mcpWindow } from "./Mcp.tsx";
import { Tree, type TreeItem } from "./Tree.tsx";
import {
  phaseGroups,
  WF_FILTERS,
  type WfFilter,
  type WfLevel,
  wfRunsHeight,
  windowed,
  Workflows,
  visibleAgents,
} from "./Workflows.tsx";
import { fuzzyScore } from "../format.ts";

/**
 * Why the list is absent when it is. Only reachable now if the composition root
 * declined to inject the fetch or the fetch itself failed — never a claim about what
 * the user has installed, which is the distinction `Skills.tsx` exists to keep.
 */
const SKILLS_NOTE =
  "the skills list could not be read from this server — GET /skills did not answer, " +
  "so this is not a claim that you have none installed";

/**
 * What a model choice DOES, said at the moment someone makes one.
 *
 * It used to say "pinned in this client only — there is no route to persist a model
 * (no PATCH /sessions/:id)". That sentence was false: the route is at
 * `server/app.ts:99` and `patchSession` has always honoured `model` and `effort`. An
 * apology for a gap that had closed is worse than no message — it tells the user to
 * stop trying, so the working feature stays unused.
 */
const MODEL_NOTE = "pinned for this conversation and set as the default for new ones";

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
  /**
   * Branch at a message and open the result — pi's `/tree` selection, which is
   * bough's fork (`historytree.ts`). `editorText` seeds the composer so a user
   * turn is edited and re-sent rather than replayed.
   */
  forkAt?: (
    sessionId: string,
    body: { atMessageId: string; exclusive?: boolean; summarizeAbandoned?: boolean },
    editorText?: string,
  ) => Promise<void>;
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
  /** The open conversation as a tree — pi's `/tree` (`historytree.ts`). */
  conversation?: TreeRow[];
  drillIn: (originId: string) => void;
  collapse: (originId: string) => void;
}

export interface PanelHandle {
  open: boolean;
  tab: PanelTab;
  /**
   * True when the command was the panel's. `App` returns immediately on true.
   *
   * `input` is the RAW KEYPRESS the command resolved from, and `panel.pick` is the
   * only thing that reads it — a digit cannot be recovered from the command name, and
   * nine commands for nine digits would be nine rows in the keymap saying the same
   * thing. `App` already reads `input` the same way for `ask.pick`. It is optional so
   * a caller that does not pass it loses only the digits.
   */
  handle: (command: Command, input?: string) => boolean;
  /**
   * The `/` filter buffer holds the keyboard.
   *
   * `App` must put this in its `KeyContext` and route `isTextInput` keypresses into
   * the buffer while it is true — without it every bare letter in the panel keeps its
   * command meaning and typing "opus" pauses a workflow on the way through.
   */
  filtering: boolean;
  /** Append to the filter buffer. `App` calls it for a text keypress while filtering. */
  filterInput: (text: string) => void;
  /** The mounted panel, or `null` when it is closed. */
  view: ReactNode;
}

export function usePanelHost(deps: PanelHostDeps): PanelHandle {
  const { store, state, rows, cols, now, controls = {}, models = [] } = deps;
  const conversation = deps.conversation ?? [];
  const { tree, drillIn, collapse } = deps;
  const [panel, setPanel] = useState<PanelState>(initialPanel);
  const [sel, setSel] = useState(0);
  const [focusDiff, setFocusDiff] = useState(false);
  // Rows scrolled into the focused file's hunks. The tab has always PRINTED "↑↓ scroll
  // the diff" and always passed `scroll` as its default 0, so the arrow keys moved the
  // file cursor that focus mode hides — a legend describing a key that did something
  // else, on the one screen where the thing being scrolled is the point.
  const [diffScroll, setDiffScroll] = useState(0);
  const [message, setMessage] = useState<string | null>(null);
  // A revert that has been asked for and not yet done. Revert deletes files, so `x`
  // arms this and ⏎ performs it; the scope is on screen in between (`Changes.tsx`).
  const [pendingRevert, setPendingRevert] = useState<PendingRevert | null>(null);
  // The workflows tab's drill-in: which run is open, how deep, and where each level's
  // cursor is. The tab used to render `level={0} detail={null}` hardcoded, so ⏎ did
  // nothing, no run ever showed its elapsed time or its cost, and the steering verbs
  // the footer computes from a run's state had no state to compute from.
  const [wfOpen, setWfOpen] = useState<string | null>(null);
  const [wfLevel, setWfLevel] = useState<WfLevel>(0);
  const [wfDetail, setWfDetail] = useState<WorkflowDetail | null>(null);
  const [wfPhaseSel, setWfPhaseSel] = useState(0);
  const [wfAgentSel, setWfAgentSel] = useState(0);
  const [wfScroll, setWfScroll] = useState(0);
  const [wfPromptOpen, setWfPromptOpen] = useState(false);
  // The `f` cycle over `WF_FILTERS`. The tab has always RENDERED a filter — the count
  // beside a phase's name says `12 running` when one is on — and the prop was hard
  // wired to `null`, so the feature existed everywhere except in the keyboard.
  const [wfFilter, setWfFilter] = useState<WfFilter>(null);
  // The `/` buffer: `filtering` is who has the keyboard, `filter` is what it holds.
  // Two fields and not one, because an empty buffer you are typing into and no buffer
  // at all are different screens and different keymaps.
  // Which run `x` has armed for a stop, mirroring the rail's `armedStop`. Cleared by
  // any move or level change below, so an arm cannot outlive the row it was about.
  const [armedStop, setArmedStop] = useState<string | null>(null);
  const [filtering, setFiltering] = useState(false);
  const [filter, setFilter] = useState("");
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

  // The filter NARROWS the list the cursor walks, and it does so here rather than in
  // the tab body: `sel` is bounded by the list's length, so a body that filtered on
  // its own would leave the cursor addressing rows that are no longer there.
  const sessions = useMemo(
    () => sessionItems(state.sessions, panel.tab === "sessions" ? filter : ""),
    [state.sessions, panel.tab, filter],
  );
  const changes = useMemo(() => changeItems(state.changes), [state.changes]);
  const entries = useMemo(() => {
    const all = modelEntries(models);
    if (panel.tab !== "model" || filter.trim() === "") return all;
    return all.filter((e) => fuzzyScore(`${e.label} ${e.detail}`, filter.trim()) > 0);
  }, [models, panel.tab, filter]);
  const shownSkills = useMemo(() => {
    if (!skills || panel.tab !== "skills" || filter.trim() === "") return skills;
    return skills.filter((s) =>
      fuzzyScore(`${s.name} ${s.description ?? ""}`, filter.trim()) > 0
    );
  }, [skills, panel.tab, filter]);

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
    // The pin's OTHER half, hydrated the same way and for a sharper reason: a frontier
    // pick PATCHes model AND effort together, so an un-hydrated `sessionEffort` sent
    // `effort: null` and a pick of a model silently cleared a thinking depth the user
    // had chosen (`effort: high` -> `None`, verified on a live row). It also decided
    // which row of the thinking-depth section wears the ●.
    sessionEffort: cfg.sessionEffort ?? asEffortChoice(state.session?.effort),
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
        .then((s) =>
          setCfg((c) =>
            c.defaultModel ? c : {
              ...c,
              defaultModel: s.defaultModel,
              // All three tiers or none: the route answers for the cheap tier and the
              // default effort too, and dropping them here is what left the cheap row
              // printing "(unset)" beside a model that was running on every round.
              cheapModel: c.cheapModel ?? s.cheapModel,
              defaultEffort: s.defaultEffort ?? c.defaultEffort,
            }
          )
        )
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

  // The opened run, re-read whenever the bus says a workflow moved. A run's header is
  // its elapsed time, its replay counts and its cost — all three go stale the moment an
  // agent settles, and a panel showing last minute's numbers for a live run is the
  // failure mode the drill-in exists to remove.
  useEffect(() => {
    if (!panel.open || panel.tab !== "workflows" || !wfOpen) return;
    let alive = true;
    void api.getWorkflow(wfOpen)
      .then((d) => alive && setWfDetail(d))
      .catch((e: unknown) => {
        if (!alive) return;
        setMessage(e instanceof Error ? e.message : String(e));
        setWfOpen(null);
        setWfLevel(0);
      });
    return () => {
      alive = false;
    };
  }, [panel.open, panel.tab, wfOpen, state.workflowSeq]);

  const wfGroups = useMemo(
    () => (wfDetail ? phaseGroups(wfDetail.workflow, wfDetail.agents) : []),
    [wfDetail],
  );
  // FILTERED, because the tab renders the filtered list: `f` narrows what is on
  // screen, and a cursor bounded by the unfiltered length would address agents that
  // are not there — `o` would then open a session belonging to a different row.
  const wfAgents = visibleAgents(
    wfGroups[Math.min(wfPhaseSel, Math.max(0, wfGroups.length - 1))]?.agents ?? [],
    wfFilter,
  );

  const items = tabLength(panel.tab, {
    sessions: sessions.length,
    tree: state.currentId ? conversation.length : tree.length,
    changes: changes.length,
    workflows: state.workflows.length,
    model: entries.length,
    mcp: mcp ? Object.keys(mcp.registry.servers).length : 0,
    skills: shownSkills?.length ?? 0,
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

  // Arriving at a tab arrives at its top, with no message, no held diff focus, no
  // armed revert (a confirm that outlives the screen it was read on is not a confirm)
  // and no run drilled into.
  useEffect(() => {
    setSel(0);
    setMessage(null);
    setFocusDiff(false);
    setDiffScroll(0);
    setPendingRevert(null);
    setArmedStop(null);
    setWfOpen(null);
    setWfLevel(0);
    setWfDetail(null);
    setWfFilter(null);
    // The filter is the tab's, not the panel's. Carrying "opus" from the model tab
    // into skills would silently hide most of the list on arrival, and the buffer
    // holding the keyboard across a tab change would eat that tab's own letters.
    setFiltering(false);
    setFilter("");
  }, [panel.tab, panel.open]);

  /**
   * The row window each list tab is PAINTING, so a digit lands where the eye is.
   *
   * `1`–`9` address rows on screen (spec §3), and which rows are on screen is decided
   * by each tab body's own budget. Rather than guess at it here, each body exports the
   * window function it renders from and this calls the same one with the same inputs —
   * so the digit printed on a row and the digit that selects it cannot drift apart.
   */
  // `rows` is the panel's slot; `body` is what `Panel` is given (its border and the
  // rows this host spends), and `bodyRows` is what a TAB BODY actually paints. The
  // floors are 1 and not 4: a floor is a claim about available space, and every one
  // of them here was false at twelve terminal rows — which is how six list rows came
  // to be painted into three (`Panel.tsx`).
  const body = Math.max(1, rows - 4);
  const bodyRows = panelBodyRows(body);
  const pickTargets = useCallback((): number[] => {
    const chrome = (message ? 1 : 0) + (filtering || filter ? 1 : 0);
    switch (panel.tab) {
      case "sessions": {
        const { start, height } = sessionsWindow(sessions.length, sel, bodyRows, chrome);
        return range(start, Math.min(sessions.length, start + height));
      }
      case "model": {
        const display = displayRows(entries, { cheapUnset: modelCfg.cheapModel === null });
        const { start, end } = modelWindow(display, sel, bodyRows, chrome);
        return visibleEntries(display, start, end);
      }
      case "skills": {
        const count = shownSkills?.length ?? 0;
        const { start, height } = skillsWindow(count, sel, bodyRows, chrome);
        return range(start, Math.min(count, start + height));
      }
      case "mcp": {
        const count = mcp ? Object.keys(mcp.registry.servers).length : 0;
        const { start, end } = mcpWindow(count, sel, bodyRows, message ? 1 : 0);
        return range(start, end);
      }
      case "workflows": {
        // Only the run list is numbered; the drill-in levels are Miller columns, not
        // a list of options.
        if (wfLevel !== 0) return [];
        const height = wfRunsHeight(bodyRows);
        if (height === 0) return [];
        // `windowed` is what `RunsList` slices with, so the digits and the rows agree
        // by calling the same function rather than by two matching calculations.
        const { slice, from } = windowed(state.workflows, sel, height);
        return range(from, from + slice.length);
      }
      default:
        // `changes`, `tree` and `theme` are not option lists — the first two are
        // drill-ins and the third previews on cursor move, so a digit that jumped
        // and affirmed would commit a theme you never saw.
        return [];
    }
  }, [
    panel.tab,
    sel,
    bodyRows,
    message,
    filtering,
    filter,
    sessions.length,
    entries,
    modelCfg.cheapModel,
    shownSkills,
    mcp,
    wfLevel,
    state.workflows,
  ]);

  /**
   * The armed revert, performed — and then SAID.
   *
   * Three things happen after the write, and the panel used to do none of them: the
   * change set is re-fetched (a destructive key that leaves the screen byte-identical
   * reads as inert, and the obvious response to an inert key is to press it again on
   * the next row), the outcome is printed in the tab, and it is also pushed into the
   * transcript so the fact outlives the panel. `reverted`/`skipped`/`failed` are the
   * server's three answers and all three are reported: a path the server declined to
   * touch must not read as one it deleted.
   */
  const performRevert = useCallback((target: PendingRevert) => {
    const id = state.currentId;
    setPendingRevert(null);
    if (!id) return setMessage("no conversation is open, so there is nothing to revert");
    // `undefined` — never an empty array — is what "the whole change set" means to
    // `POST /changes/revert`; an empty selection is refused there on purpose.
    const paths = target.scope === "all" ? undefined : [target.item.file.path];
    void api.revertChanges(id, paths)
      .then((outcome) => {
        const parts = [
          outcome.reverted.length
            ? `reverted ${outcome.reverted.join(", ")}`
            : "nothing was reverted",
          ...(outcome.skipped.length
            ? [`not in this change set: ${outcome.skipped.join(", ")}`]
            : []),
          ...outcome.failed.map((f) => `failed ${f.path}: ${f.error}`),
        ];
        const line = parts.join(" · ");
        setMessage(line);
        // `record`, NOT `notify`. A notice expires after `NOTICE_TTL_MS`, so ten
        // seconds after deleting a file there was no record anywhere that it had
        // happened. `record` prints the same line AND appends a permanent mark to the
        // transcript (`store.ts`) — one method, both halves, so a destructive path
        // cannot do the reasonable half alone.
        store.record(line);
        setSel(0);
        setFocusDiff(false);
        return store.refreshChanges();
      })
      .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
  }, [state.currentId, store]);

  /**
   * ⏎ on the active tab. One place, so a tab's affirmative is one line of code.
   *
   * `at` defaults to the cursor and is passed explicitly by `panel.pick`: a digit
   * moves the cursor and affirms in one gesture, and `setSel` does not take effect
   * until the next render, so the row has to travel with the call.
   */
  const confirm = useCallback((summarize = false, at = sel) => {
    switch (panel.tab) {
      case "sessions": {
        const item = sessions[at];
        if (!item) return;
        dispatch({ type: "close" });
        return void store.open(item.session.id);
      }
      case "tree": {
        // With a conversation open this tab IS that conversation, so ⏎ means
        // "go back to this turn" — pi's selection rules, resolved by
        // `selectionFor`: a user turn cuts BEFORE itself and hands its text to
        // the composer so you edit and re-send; anything else cuts inclusive and
        // leaves the composer empty; a branch row is a session, so open it.
        if (state.currentId) {
          const row = conversation[at];
          if (!row) return;
          const choice = selectionFor(row, state.thread);
          dispatch({ type: "close" });
          if ("open" in choice) return void store.open(choice.open);
          return void controls.forkAt?.(
            state.currentId,
            { ...choice.fork, ...(summarize ? { summarizeAbandoned: true } : {}) },
            choice.editorText,
          );
        }
        const item = tree[at];
        if (!item || item.type !== "session") return;
        dispatch({ type: "close" });
        return void store.open(item.session.id);
      }
      case "changes":
        // With a revert armed ⏎ is that revert's yes — the scope has been printed and
        // read. With nothing armed it is NOT revert: revert deletes untracked files and
        // restores tracked ones, and ⏎ is the key a cursor lands on. That gives the
        // diff the whole tab instead.
        if (pendingRevert) return void performRevert(pendingRevert);
        setDiffScroll(0);
        return setFocusDiff((v) => !v);
      case "workflows": {
        // The Miller-column drill-in: runs → phases → that phase's agents → one agent.
        // Opening a run is what fetches its detail, and the detail is what carries the
        // elapsed time, the replay accounting (spec §8) and the cost.
        if (wfLevel === 0) {
          const run = state.workflows[at];
          if (!run) return;
          setWfOpen(run.id);
          setWfDetail(null);
          setWfLevel(1);
          setWfPhaseSel(0);
          setWfAgentSel(0);
          setWfScroll(0);
          return;
        }
        if (wfLevel === 1) {
          setWfAgentSel(0);
          return setWfLevel(2);
        }
        if (wfLevel === 2) {
          if (wfAgents.length === 0) return;
          setWfScroll(0);
          setWfPromptOpen(false);
          return setWfLevel(3);
        }
        return setWfPromptOpen((v) => !v);
      }
      case "model": {
        const entry = entries[at];
        if (!entry) return;
        const next = chooseEntry(modelCfg, entry);
        setCfg(next);
        setMessage(MODEL_NOTE);
        // `null` CLEARS the pin — the picker's "adaptive" row means "let the provider
        // decide", and sending the string "default" would pin the word instead. The
        // server publishes `session.updated` and the reducer applies it, so nothing is
        // reconciled here.
        return void store.setModel({
          model: next.sessionModel,
          effort: next.sessionEffort === "default" || next.sessionEffort === null
            ? null
            : next.sessionEffort,
        });
      }
      case "mcp": {
        const name = mcp ? Object.keys(mcp.registry.servers).sort()[at] : undefined;
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
    pendingRevert,
    performRevert,
    wfLevel,
    wfAgents.length,
    conversation,
    controls.forkAt,
  ]);

  /**
   * A steering verb, applied to the run in hand — the one drilled into, or the one the
   * cursor is on at the list level. Stopping a run you are looking at must not require
   * backing out to the list first, and the list's own footer names these keys.
   */
  const steer = useCallback((run: ((id: string) => Promise<void>) | undefined, verb: string) => {
    const target = wfOpen
      ? (state.workflows.find((w) => w.id === wfOpen) ?? { id: wfOpen, name: wfOpen, status: "" })
      : state.workflows[sel];
    if (!target) return;
    if (!run) return setMessage(`${verb} is not wired up in this client yet`);
    void run(target.id)
      .then(() => {
        // Spec §7: a destructive action is RECORDED, not toasted. `store.stopUnit`
        // already records the same act when it is stopped from the rail; this is the
        // other door to it, and a door that leaves no trace is how the revert
        // regression happened. Pause/resume/relaunch are reversible and stay quiet.
        if (verb === "stop") store.record(`stopped workflow ${target.name || target.id}`);
        return store.refreshWorkflows();
      })
      .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
  }, [state.workflows, sel, store, wfOpen]);

  /**
   * `x` in the workflows tab, armed then confirmed — the rail's idiom, because it is
   * the same act through a second door.
   *
   * A single press used to stop a running multi-agent run outright: no scope on
   * screen, no confirmation, nothing in the transcript. The rail's `x` on the very
   * same run armed first and recorded after, and the changes tab's `x` printed its
   * blast radius, which left this the one destructive verb in the panel that inferred
   * consent (spec §7). A run that is not running needs no ceremony — stopping a
   * finished run changes nothing — so only a live one arms.
   */
  const armStop = useCallback(() => {
    const target = wfOpen
      ? state.workflows.find((w) => w.id === wfOpen)
      : state.workflows[sel];
    const live = target ? target.status === "running" || target.status === "paused" : false;
    if (!live || armedStop === target?.id) {
      setArmedStop(null);
      return steer(controls.stopWorkflow, "stop");
    }
    setArmedStop(target!.id);
    setMessage(
      `x again to stop ${target!.name || target!.id} — agents in flight are lost, ` +
        `and journaled work is kept`,
    );
  }, [state.workflows, sel, wfOpen, armedStop, steer, controls.stopWorkflow]);

  /**
   * `x` in the changes tab. Arms a revert, or widens an armed one to the whole change
   * set — the escalation the second press performs is spelled out on screen before it
   * is available, and it is still one more ⏎ away from happening.
   */
  const armRevert = useCallback(() => {
    if (changes.length === 0) return;
    if (pendingRevert?.scope === "file") {
      if (changes.length > 1) setPendingRevert({ scope: "all" });
      return;
    }
    if (pendingRevert) return; // already at the widest scope there is
    const item = changes[sel];
    if (item) setPendingRevert({ scope: "file", item });
  }, [changes, sel, pendingRevert]);

  /** ← / esc inside a drilled-in tab: one level, not the whole panel. */
  const back = useCallback((): boolean => {
    if (pendingRevert) {
      setPendingRevert(null);
      return true;
    }
    // An armed stop is a state, so esc unwinds it before it unwinds anything else —
    // the same rule the armed revert follows one line up.
    if (armedStop) {
      setArmedStop(null);
      setMessage(null);
      return true;
    }
    if (panel.tab === "changes" && focusDiff) {
      setFocusDiff(false);
      setDiffScroll(0);
      return true;
    }
    if (panel.tab === "workflows" && wfLevel > 0) {
      const next = (wfLevel - 1) as WfLevel;
      setWfLevel(next);
      if (next === 0) {
        setWfOpen(null);
        setWfDetail(null);
      }
      return true;
    }
    return false;
  }, [pendingRevert, armedStop, panel.tab, wfLevel, focusDiff]);

  /**
   * A character typed into the `/` buffer.
   *
   * `App` routes `isTextInput` keypresses here while `filtering` — the keymap makes
   * that safe by returning null for every bare letter and digit in the panel when
   * `panelFiltering` is set, so a user typing "opus" no longer pauses a workflow on
   * the `p`. Narrowing resets the cursor: row 4 of the old list is not row 4 of the
   * new one.
   */
  const filterInput = useCallback((text: string) => {
    if (!filtering) return;
    setFilter((f) => f + text);
    setSel(0);
  }, [filtering]);

  /** Move the cursor and disarm anything read against the row it was on. */
  const moveTo = useCallback((next: number) => {
    setSel(Math.max(0, Math.min(Math.max(0, items - 1), next)));
    if (pendingRevert) setPendingRevert(null);
    // Same rule for the workflow stop: `x` armed against THIS row, and a cursor that
    // has moved means the sentence on screen is about a run you are no longer on.
    if (armedStop) {
      setArmedStop(null);
      setMessage(null);
    }
  }, [items, pendingRevert, armedStop]);

  const handle = useCallback((command: Command, input = ""): boolean => {
    // Escape unwinds ONE state (the teardown's rule, and the panel's own): an armed
    // revert and a drilled-in run are states, so esc backs out of them before it backs
    // out of the panel. Without this, cancelling a confirm also closed the tab it was
    // asked in, and there was no way back up a workflow's levels at all.
    if (panel.open && command === "panel.close" && back()) return true;
    const action = panelActionFor(command);
    if (action) {
      // A chord opens the panel from outside it; everything else needs it open.
      const fromOutside = action.type === "toggle" || action.type === "jump";
      if (!panel.open && !fromOutside) return false;
      if (action.type === "move") {
        // One cursor per level. Inside a run the arrow keys walk the phase list, then
        // that phase's agents, then the open agent's detail — the same key meaning the
        // list it is looking at, which is what a Miller column is.
        if (panel.tab === "workflows" && wfLevel > 0) {
          const clamp = (i: number, n: number) => Math.max(0, Math.min(Math.max(0, n - 1), i));
          if (wfLevel === 1) setWfPhaseSel((i) => clamp(i + action.delta, wfGroups.length));
          else if (wfLevel === 2) setWfAgentSel((i) => clamp(i + action.delta, wfAgents.length));
          else setWfScroll((i) => Math.max(0, i + action.delta));
        } else if (panel.tab === "changes" && focusDiff) {
          // Focus mode gave the whole tab to one file's hunks; ↑↓ scrolls THEM.
          // `Changes` clamps to the body length, so an overrun cannot scroll past it.
          setDiffScroll((i) => Math.max(0, i + action.delta));
        } else {
          // A moved cursor is a different file, so a confirm read against the old one
          // no longer applies. Disarm rather than re-target — `moveTo` does both.
          moveTo(sel + action.delta);
        }
      }
      dispatch(action);
      if (action.type === "confirm") confirm();
      // `s` is the TREE's "branch, carrying a summary" — its binding says so, and
      // the dispatcher ignored the tab. Pressed anywhere else it ran the tab's
      // ordinary commit: `s` in the model picker pinned a model, which is a bare
      // letter silently changing a setting in a panel that has no `s` in its legend.
      if (action.type === "confirmSummarize" && panel.tab === "tree") confirm(true);
      return true;
    }
    if (!panel.open) return false;
    switch (command) {
      case "move.in": {
        // → drills in wherever there is an in: a delegated session in the tree, the
        // next Miller column in a workflow run, one file's whole diff in changes —
        // which is what the changes legend has always said → does.
        if (panel.tab === "workflows" && wfLevel < 3) {
          confirm();
          return true;
        }
        if (panel.tab === "changes") {
          if (changes.length > 0) setFocusDiff(true);
          return true;
        }
        const item = tree[sel];
        if (panel.tab !== "tree" || !item) return true;
        drillIn(item.type === "session" ? item.session.id : item.originId);
        return true;
      }
      case "move.out": {
        if (back()) return true;
        const item = tree[sel];
        if (panel.tab !== "tree" || !item) return true;
        collapse(item.type === "session" ? item.session.id : item.originId);
        return true;
      }
      // ---- a screenful at a time (spec §3: a list you can only walk one row at a
      // time is a list whose far end does not exist — the model tab is 32 rows in a
      // ~20-row viewport). The page is the tab's OWN viewport, not a constant.
      case "move.pageUp":
        moveTo(sel - Math.max(1, bodyRows - 2));
        return true;
      case "move.pageDown":
        moveTo(sel + Math.max(1, bodyRows - 2));
        return true;

      // ---- the digit, read off the keypress (see `PanelHandle.handle`) ----
      case "panel.pick": {
        const n = Number(input);
        if (!Number.isInteger(n) || n < 1 || n > 9) return true;
        const target = pickTargets()[n - 1];
        // A digit past the end of the visible window is not an error and not a jump
        // to the nearest row: it addresses a row that is not on screen, so it does
        // nothing at all.
        if (target === undefined) return true;
        setSel(target);
        setPendingRevert(null);
        // The cursor and the affirmative are ONE gesture — that is the whole point of
        // a numbered list. `confirm` reads `sel` from this render's closure, so the
        // index is passed rather than waiting for the state it just set.
        confirm(false, target);
        return true;
      }

      // ---- the `/` buffer (`FILTER_TABS` only; the keymap enforces which) ----
      case "panel.filter":
        setFiltering(true);
        return true;
      case "panel.filterBack":
        setFilter((f) => f.slice(0, -1));
        setSel(0);
        return true;
      case "panel.filterExit":
        // Esc unwinds ONE level: it drops the filter and keeps the panel open. The
        // keymap orders this ahead of `panel.close`, so the next esc closes.
        setFiltering(false);
        setFilter("");
        setSel(0);
        return true;

      // `p`/`P`/`r`/`x` are the WORKFLOWS tab's verbs and used to reach the dispatcher
      // from every tab, steering `state.workflows[sel]` — the row a cursor in an
      // unrelated list happened to be on. The hand-written `if (panel.tab !==
      // "workflows") return true` guards that fixed it are GONE: the bindings carry
      // `tab: ["workflows"]` now, so `lookup` never resolves them elsewhere, and two
      // places deciding a binding's scope is two places that can disagree.
      case "wf.pause":
        steer(controls.pauseWorkflow, "pause");
        return true;
      case "wf.resume":
        steer(controls.resumeWorkflow, "resume");
        return true;
      case "wf.stop":
        armStop();
        return true;
      case "wf.rerun":
        steer(controls.rerunWorkflow, "relaunch");
        return true;
      case "wf.script":
        // Level 4 — the script, and the mirror path that is the whole steering loop's
        // edit target. Nothing could reach it: `e` was never bound, so `Workflows.tsx`
        // filtered it out of its own footer rather than advertise a dead key.
        if (!wfOpen || !wfDetail) {
          return void setMessage("open a run first — e shows that run's script"), true;
        }
        // The refusal above is only true until a run IS open, and a message that
        // outlives the state it described reads as advice about the screen you are
        // looking at. Every arm that succeeds clears it.
        setMessage(null);
        setWfScroll(0);
        setWfLevel(4);
        return true;
      case "wf.filter":
        setMessage(null);
        setWfFilter((f) => WF_FILTERS[(WF_FILTERS.indexOf(f) + 1) % WF_FILTERS.length] ?? null);
        setWfAgentSel(0);
        return true;
      case "wf.openAgent": {
        // `agentDetailRows` prints "session <id> — o opens it" on every agent that ran.
        // It was a promise for a key that did not exist; this is the key.
        const agent = wfAgents[wfAgentSel];
        if (!agent?.sessionId) {
          return void setMessage(
            "no session on this agent — the call was replayed from the journal",
          ),
            true;
        }
        dispatch({ type: "close" });
        return void store.open(agent.sessionId), true;
      }

      // ---- changes: the two revert scopes, each its own binding ----
      case "changes.revert":
        // Arms; nothing is written until ⏎ (`performRevert`). `x` reached this tab
        // only because it WAS `wf.stop` and this dispatcher re-routed it by hand.
        armRevert();
        return true;
      case "changes.revertAll":
        // `X` could not reach this tab at all. The widest scope, still one ⏎ away,
        // with the blast radius printed in between (`Changes.tsx`).
        if (changes.length > 0) setPendingRevert({ scope: "all" });
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
    back,
    armRevert,
    pendingRevert,
    focusDiff,
    changes.length,
    wfLevel,
    wfGroups.length,
    wfAgents,
    wfAgentSel,
    wfOpen,
    wfDetail,
    bodyRows,
    moveTo,
    pickTargets,
  ]);

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
          // Built into `Sessions.tsx` from the start and never passed — the tab drew
          // a `/ filter` line for a buffer nothing could fill.
          filter,
          filtering,
        }}
        changes={{
          set: state.currentId ? state.changes : NO_SESSION_CHANGES,
          ...(state.currentId ? {} : { hint: null }),
          items: changes,
          selected: sel,
          scroll: diffScroll,
          rows: body,
          focused: focusDiff,
          message,
          pending: pendingRevert,
        }}
        model={{ cfg: modelCfg, entries, selected: sel, rows: body, message, filter, filtering }}
        mcp={{ status: mcp, selected: sel, message }}
        skills={{
          skills: shownSkills,
          selected: sel,
          sources: skillSources,
          note: skills === null && !loadingSkills ? SKILLS_NOTE : undefined,
          filter,
          filtering,
        }}
        theme={{ preview }}
      >
        {panel.tab === "tree"
          ? (
            // With a conversation open the tree is THAT conversation — pi's
            // `/tree`. With none open there is nothing to branch, so it falls back
            // to the session lineage, which is what this tab used to be.
            state.currentId
              // `bodyRows`, not `body`: these two tabs arrive as `children`, so
              // `Panel` cannot subtract its own chrome for them the way it does for
              // the tabs it mounts itself. They were handed two rows they did not
              // have, every time.
              ? <ConversationTree rows={conversation} selected={sel} height={bodyRows} />
              : <Tree items={tree} selected={sel} rows={bodyRows} />
          )
          : (
            <>
              {/*
                The workflows tab had no place to say anything, so every steering
                failure — "stop is not wired up in this client yet", a server error on
                pause — was computed and then dropped. The other tabs render `message`;
                this one now does too.
              */}
              {message
                ? <text fg={palette.warn} wrapMode="none">{message}</text>
                : null}
              <Workflows
                runs={state.workflows}
                sel={sel}
                level={wfLevel}
                detail={wfDetail}
                phaseSel={wfPhaseSel}
                agentSel={wfAgentSel}
                scroll={wfScroll}
                filter={wfFilter}
                promptOpen={wfPromptOpen}
                rows={bodyRows - (message ? 1 : 0)}
                cols={cols}
                now={now}
              />
            </>
          )}
      </Panel>
    )
    : null;

  return {
    open: panel.open,
    tab: panel.tab,
    handle,
    filtering,
    filterInput,
    view,
  };
}

/** `[from, to)` as indices. The digits address a contiguous run of visible rows. */
function range(from: number, to: number): number[] {
  const out: number[] = [];
  for (let i = Math.max(0, from); i < to; i++) out.push(i);
  return out;
}

/** How many rows the cursor may walk on this tab. Kept out of the hook body. */
function tabLength(tab: PanelTab, counts: Record<PanelTab, number>): number {
  return counts[tab];
}
