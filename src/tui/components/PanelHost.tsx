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
import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { McpStatus } from "../../mcp/status.ts";
import type { ModelRow } from "../../llm/client.ts";
import { api, type SessionRow, type WorkflowDetail } from "../api.ts";
import { type ForestInput, forestRows, rewindIndex, selectionFor } from "../forest.ts";
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
import { hasStaticAuth } from "./Mcp.tsx";
import {
  asEffortChoice,
  chooseEntry,
  displayRows,
  isActive,
  type ModelConfig,
  modelEntries,
  modelWindow,
  visibleEntries,
} from "./ModelPicker.tsx";
import { type SkillRow, type SkillSourceRow, skillsWindow } from "./Skills.tsx";
import { mcpWindow } from "./Mcp.tsx";
import { forestWindow, Tree } from "./Tree.tsx";
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
 *
 * The second half of it — "and set as the default for new ones" — was then ALSO
 * false for a while, in the other direction: nothing wrote a default, because
 * `/model-settings` was GET-only and `ctx.model` is `BOUGH_MODEL` frozen at server
 * start. The choice survived one conversation and the next reverted. `store.setModel`
 * writes both scopes now (`PUT /model-settings` + `PATCH /sessions/:id`), so the
 * sentence is finally true. Both halves of it are load-bearing; check the writes
 * before editing the words.
 */
const MODEL_NOTE = "pinned for this conversation and set as the default for new ones";

/**
 * A registry name derived from a server URL.
 *
 * Registration asked for a name AND a URL, which is one field more than the user
 * has: the name is bough's own label for the thing, and nobody arrives wanting to
 * choose it. `https://mcp.linear.app/sse` → `linear`. The leading `mcp.` and the
 * public suffix are dropped because every remote MCP server is called `mcp.<x>.com`
 * and a registry of `mcp-linear-app`, `mcp-notion-com` reads like a hostname dump.
 * Collisions get a numeric suffix rather than silently overwriting a server that is
 * already registered and possibly already authorized.
 */
export function nameFromUrl(raw: string, taken: readonly string[] = []): string {
  let host: string;
  try {
    host = new URL(raw).hostname;
  } catch {
    return "";
  }
  const parts = host.split(".").filter((p) => p && p !== "mcp" && p !== "www" && p !== "api");
  // Drop the TLD, and the second-level part of a `co.uk`-shaped suffix with it.
  const base = (parts.length > 2 && parts.at(-2)!.length <= 3
    ? parts.slice(0, -2)
    : parts.slice(0, -1)).at(-1) ?? parts[0] ?? host;
  const slug = base.toLowerCase().replace(/[^a-z0-9-]/g, "-").replace(/^-+|-+$/g, "");
  if (!slug) return "";
  if (!taken.includes(slug)) return slug;
  for (let i = 2; i < 100; i++) if (!taken.includes(`${slug}-${i}`)) return `${slug}-${i}`;
  return slug;
}

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
/**
 * How long the tab waits on the browser half of an OAuth flow: 2s × 150 = 5 minutes.
 * Long enough to find the window, log in and approve; bounded so an abandoned flow
 * does not leave a timer running for the life of the process.
 */
const AUTH_POLL_MS = 2000;
const AUTH_POLLS = 150;

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
  /**
   * `POST /sessions/:id/extract` — copy this turn and every later turn of its thread
   * into a fresh ROOT, and open it. A copy, never a move: the source keeps everything
   * (`history/extract.ts`).
   */
  extractFrom?: (sessionId: string, picks: { messageId: string }[]) => Promise<void>;
  /**
   * `POST /sessions/:id/move-into` — copy this turn and every later turn of its thread
   * onto the END of the open conversation. Extract's sibling, and a copy just as much:
   * the source keeps everything (`history/move.ts`).
   */
  moveIntoOpen?: (
    targetId: string,
    sourceId: string,
    picks: { messageId: string }[],
  ) => Promise<void>;
  /** `GET /sessions?originId=` — delegated children, for the tree and the rail. */
  listChildren?: (originId: string) => Promise<SessionRow[]>;
  /** `GET /mcp/servers?session=` — re-read on every entry, never cached. */
  loadMcp?: (sessionId?: string) => Promise<McpStatus>;
  /** `POST /mcp/servers/:name/{enable,disable}` — the grant itself. */
  setMcpEnabled?: (name: string, on: boolean, sessionId: string) => Promise<unknown>;
  /**
   * `POST /mcp/servers/:name/auth` — begin OAuth. Returns the URL a human opens.
   *
   * Never opens a browser: a headless server that shells out to one hangs when
   * there is no browser, and the model must never be handed a URL to "click"
   * (`mcp/oauth.ts`). The URL comes back here and this tab prints it.
   */
  beginMcpAuth?: (name: string) => Promise<{ status: string; authorizationUrl?: string }>;
  /** `DELETE /mcp/servers/:name/auth` — drop stored credentials. */
  clearMcpAuth?: (name: string) => Promise<unknown>;
  /** `DELETE /mcp/servers/:name` — remove the registration itself. */
  deleteMcpServer?: (name: string) => Promise<unknown>;
  /**
   * `POST /mcp/servers/:name/connect` — connect now and report. Proof, not a
   * grant: the panel otherwise states only what WILL be tried.
   */
  connectMcpServer?: (
    name: string,
    sessionId: string,
  ) => Promise<{ connected: boolean; error?: string; tools?: { name: string }[] }>;
  /**
   * `POST /mcp/servers/:name/restart` — drop this session's child process and start a
   * new one. `restarted: false` means there was nothing running to replace.
   */
  restartMcpServer?: (name: string, sessionId: string) => Promise<{ restarted: boolean }>;
  /** `GET /mcp/servers/:name/auth` — polled after the browser half of the flow. */
  mcpAuthStatus?: (name: string) => Promise<{ authorized: boolean }>;
  /** `PUT /mcp/servers/:name` — register a definition. Registering grants nothing. */
  putMcpServer?: (name: string, config: unknown) => Promise<unknown>;
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
  /**
   * THE forest's raw material: every conversation, the threads that have been
   * fetched, and what is expanded. Folded into rows here rather than in `App`
   * because the `/` filter that narrows them lives here (`forest.ts`).
   */
  forest: Omit<ForestInput, "currentId" | "filter" | "userOnly">;
  /** Show a conversation's turns. Fetches its thread if this is the first time. */
  expand: (sessionId: string) => void;
  /** Hide them again. */
  collapseTurns: (sessionId: string) => void;
  /** Reveal a collapsed delegated fan-out (spec §4). */
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
   * A wheel tick, offered to the panel before the transcript takes it. True when the panel
   * consumed it.
   *
   * Only a FOCUSED DIFF consumes one. A tick over a list would move the cursor, and the
   * cursor is what a revert key targets — a scroll gesture must never change that.
   */
  scrollBy: (rows: number) => boolean;
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
  /**
   * Open the panel straight onto one run's view, by id.
   *
   * The transcript's workflow card is the entry point this exists for: a click on a
   * card must land on THAT run, not on the workflows tab with the cursor wherever it
   * happened to be. Unknown ids still open the tab — a run purged out from under a
   * card should show the list, not nothing.
   */
  openRun: (runId: string) => void;
  /** The mounted panel, or `null` when it is closed. */
  view: ReactNode;
}

export function usePanelHost(deps: PanelHostDeps): PanelHandle {
  const { store, state, rows, cols, now, controls = {}, models = [] } = deps;
  const { expand, collapseTurns, drillIn, collapse } = deps;
  const [panel, setPanel] = useState<PanelState>(initialPanel);
  const [sel, setSel] = useState(0);
  /**
   * The row an ARRIVAL should land on, when it is not the top.
   *
   * "Arriving at a tab arrives at its top" is the rule below and it is right for
   * every ordinary entry — but `esc esc` is not one: it names the row it is going
   * to. A ref rather than state because it is consumed by the very effect that
   * would otherwise overwrite it, and one render later it is nobody's business.
   */
  const landOn = useRef<number | null>(null);
  /**
   * A row to keep the cursor on across a list that is about to be rebuilt, addressed by
   * ID because the INDEX is exactly what changes. Set when `/` closes; see there.
   */
  const landOnId = useRef<string | null>(null);
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
  /** A run the transcript asked for, waiting for the workflows tab to be on screen. */
  const [pendingRun, setPendingRun] = useState<string | null>(null);
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
  /**
   * Conversations whose MESSAGES match the tree's `/` filter.
   *
   * The keymap has always said `/` "in the tree, searches every message", and the `not
   * bound` list points `^r` at it — but the filter only ever compared titles and
   * workspaces, so `/compound` answered `nothing matches "compound"` while
   * `GET /search?q=compound` returned five hits in three conversations. Built, endpoint
   * and client method and all, and never called.
   *
   * Session ids only: a row is either reachable or it is not, and that is the question
   * the switcher is asking. The snippet the server returns is the next thing to surface
   * and is deliberately not guessed at here.
   */
  const [searchHits, setSearchHits] = useState<
    { q: string; ids: readonly string[]; messages: readonly string[] }
  >({ q: "", ids: [], messages: [] });
  useEffect(() => {
    const q = panel.tab === "tree" ? filter.trim() : "";
    if (q.length < 2) {
      setSearchHits({ q: "", ids: [], messages: [] });
      return;
    }
    // Debounced: this fires per keystroke, and FTS over every transcript is not free.
    const timer = setTimeout(() => {
      void store.searchSessions(q).then((r) => {
        setSearchHits({ q, ids: r.sessions, messages: r.messages });
        // EXPAND each hit, which is also what FETCHES its thread (`App`'s `expand`). A
        // conversation's turns only render when it is open, so marking the matching turn and
        // leaving the row collapsed would be marking something nobody can see — and spreading the
        // ids into `expanded` here would open rows whose threads had never been fetched, which
        // renders as a conversation with no turns at all.
        for (const id of r.sessions) deps.expand(id);
      });
    }, 180);
    return () => clearTimeout(timer);
  }, [filter, panel.tab, store, deps.expand]);
  /**
   * What the typed buffer IS. `filtering` means "the panel has the text keyboard";
   * this says what happens on ⏎.
   *
   * One buffer, two jobs, because they are the same gesture and the keymap already
   * makes bare letters safe while it is open (`panelFiltering`). A second parallel
   * input path would need its own guard and would be the second place to get that
   * wrong. `filter` is only handed to a tab body when this is `"filter"` — an MCP
   * URL half-typed must never narrow the sessions list underneath it.
   */
  const [entryKind, setEntryKind] = useState<"filter" | "mcpUrl">("filter");
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
  const tree = useMemo(
    () =>
      forestRows({
        ...deps.forest,
        currentId: state.currentId,
        // Only when the buffer belongs to THIS tab: an MCP URL half-typed must never
        // narrow the conversation list underneath it.
        filter: panel.tab === "tree" ? filter : "",
        ...(searchHits.q === filter.trim() && searchHits.ids.length > 0
          ? {
            matchedSessions: searchHits.ids,
            matchedMessages: searchHits.messages,
          }
          : {}),
      }),
    [deps.forest, state.currentId, panel.tab, filter, searchHits],
  );
  // Read by the arrival effect, which must not re-run when the rows change — only
  // when the tab does.
  const treeRef = useRef(tree);
  treeRef.current = tree;
  const threads = deps.forest.threads;
  const changes = useMemo(() => changeItems(state.changes), [state.changes]);
  const entries = useMemo(() => {
    const all = modelEntries(models);
    if (panel.tab !== "model" || filter.trim() === "") return all;
    return all.filter((e) => fuzzyScore(`${e.label} ${e.detail}`, filter.trim()) > 0);
  }, [models, panel.tab, filter]);
  // Same reason as `treeRef`: the tab-arrival effect runs before the render that would
  // rebuild these, and it needs the CURRENT rows to find the one already in force.
  const entriesRef = useRef(entries);
  entriesRef.current = entries;
  const cfgRef = useRef<ModelConfig>(cfg);
  /** Set on arrival at the model tab; cleared once the active row can be found. */
  const landOnActive = useRef(false);
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
  // The hydrated config, for the arrival effect above — `cfg` alone lacks the session pin
  // and the effective-model fallback, so a picker landing on `isActive(cfg, …)` would miss
  // the very row the ● is on.
  cfgRef.current = modelCfg;

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
    tree: tree.length,
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

  // The transcript card's door into the run view. Jumps the tab and parks the id for
  // the effect above to apply — see there for why it cannot be done in one step.
  const openRun = useCallback((runId: string) => {
    setPendingRun(runId);
    setPanel(reducePanel(panel, { type: "jump", tab: "workflows" }, { theme: preview }));
  }, [panel, preview]);

  // Applying `landOnId`, once the rows it names exist. Separate from the parking so the
  // widened list is the one searched: `panel.filterExit` runs before the re-render that
  // rebuilds `tree`, so there is nothing to find at the moment the id is recorded.
  useEffect(() => {
    const id = landOnId.current;
    if (id === null) return;
    const at = tree.findIndex((r) => r.id === id);
    landOnId.current = null;
    if (at >= 0) setSel(at);
  }, [tree]);

  // Arriving at a tab arrives at its top, with no message, no held diff focus, no
  // armed revert (a confirm that outlives the screen it was read on is not a confirm)
  // and no run drilled into.
  useEffect(() => {
    // ARRIVING AT THE TREE LANDS ON THE CONVERSATION YOU ARE IN, not on row 0. The
    // tree is the switcher, and a switcher that opens somewhere else makes the user's
    // first job finding themselves — worse once branches nest, because the row is
    // then inside another conversation entirely. Every other tab keeps "arrive at the
    // top": their lists have no you-are-here.
    // ARRIVING LANDS ON WHAT IS ALREADY TRUE, where a tab has such a row.
    //
    // The tree lands on the conversation you are in (see below). The MODEL tab did not: it
    // opened with the cursor on row 1 while the ● sat on row 5, so `^o` then ⏎ — the most
    // natural pair of keys in a picker — silently switched the frontier model to whatever
    // happened to be listed first. A migrant persona hit exactly that. `entriesRef` is read
    // rather than `entries` because this effect runs before the render that would rebuild it.
    const here = panel.tab === "tree"
      ? treeRef.current.findIndex((r) => r.kind === "session" && r.current)
      : -1;
    // The model tab's answer is not known YET: arriving is what fetches the settings, so at
    // this instant no row is active and a `findIndex` here returns -1 every time. Parked for
    // the effect below, which fires when the config lands.
    landOnActive.current = panel.tab === "model";
    setSel(landOn.current ?? (here >= 0 ? here : 0));
    landOn.current = null;
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
   * Land on the model already in force, once the arrival fetch has told us which it is.
   *
   * DECLARED AFTER the arrival effect, because React runs effects in declaration order and
   * the flag below is set there: declared first, this ran before the flag existed and the
   * arrival effect then reset the cursor to 0 behind it.
   *
   * Separate from that effect because the answer arrives LATER because the answer arrives LATER: `^o` opened with the
   * cursor on row 1 while the ● sat on row 5, so `^o` then ⏎ — the most natural pair of keys
   * in a picker — silently switched the frontier model to whatever was listed first. Cleared
   * as soon as it fires, so it can never fight the cursor afterwards.
   */
  useEffect(() => {
    if (!landOnActive.current || !panel.open || panel.tab !== "model") return;
    // WAIT FOR THE ANSWER. Until the arrival fetch lands, `modelCfg.defaultModel` falls back
    // to the first row of the catalog — so an ungated version of this "landed" immediately,
    // on row 1, marked itself done, and never corrected when the real model arrived. Which
    // is the bug it was written to fix, reproduced from the other side.
    if (!cfg.defaultModel && !cfg.sessionModel) return;
    const at = entries.findIndex((e) => isActive(modelCfg, e));
    if (at < 0) return;
    landOnActive.current = false;
    setSel(at);
  }, [entries, modelCfg, cfg.defaultModel, cfg.sessionModel, panel.open, panel.tab]);

  /**
   * Apply a pending `openRun` once the workflows tab is actually showing.
   *
   * Two-step and not one because the reset effect directly above clears `wfOpen` on
   * every tab change: setting the tab and the run in the same handler means the reset
   * runs afterwards and throws the run away, landing on the list with the cursor
   * wherever it was. Declared AFTER that effect so it runs after it in the same
   * commit, which is what makes "open the tab, then drill in" one frame.
   */
  useEffect(() => {
    if (!pendingRun || !panel.open || panel.tab !== "workflows") return;
    const at = state.workflows.findIndex((w) => w.id === pendingRun);
    setPendingRun(null);
    // An id with no run — purged, or from another session — leaves the tab open on
    // the list rather than drilling into nothing.
    if (at < 0) return;
    setSel(at);
    setWfOpen(pendingRun);
    setWfDetail(null);
    setWfLevel(1);
    setWfPhaseSel(0);
    setWfAgentSel(0);
    setWfScroll(0);
  }, [pendingRun, panel.open, panel.tab, state.workflows]);

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
      case "tree": {
        // The SAME window the tab paints — `Tree.tsx` exports it for exactly this,
        // so a digit cannot address a row that is off screen.
        const { start, height } = forestWindow(tree.length, sel, bodyRows, chrome);
        return range(start, Math.min(tree.length, start + height));
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
    tree.length,
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
  /** The registry name under the cursor, in the same sort order the tab paints. */
  const mcpNameAt = useCallback(
    (at: number): string | undefined =>
      mcp ? Object.keys(mcp.registry.servers).sort()[at] : undefined,
    [mcp],
  );

  // Always a promise, even with no fetch injected, so callers can chain without
  // each one re-deciding what "the capability is absent" means.
  const refreshMcp = useCallback(
    async (): Promise<void> => {
      const next = await controls.loadMcp?.(state.currentId ?? undefined);
      if (next) setMcp(next);
    },
    [controls, state.currentId],
  );

  /**
   * Wait out the browser half of the OAuth flow.
   *
   * The redirect lands on bough's own callback (`GET /mcp/oauth/callback`), which
   * stores the tokens — but nothing tells this client, because the browser is a
   * different process entirely. Without this the tab sits on "open this URL"
   * forever and the user has to guess when to press a key to find out. Polling is
   * the honest mechanism: the flow finishes in a browser bough does not own.
   *
   * Bounded, because a user who abandons the flow must not leave a timer running for
   * the life of the process.
   */
  const pollAuth = useCallback(async (name: string) => {
    if (!controls.mcpAuthStatus) return;
    for (let i = 0; i < AUTH_POLLS; i++) {
      await new Promise((r) => setTimeout(r, AUTH_POLL_MS));
      let ok = false;
      try {
        ok = (await controls.mcpAuthStatus(name)).authorized;
      } catch {
        continue; // a blip mid-flow is not a failed flow
      }
      if (ok) {
        setMessage(`${name} is authorized — ⏎ grants it in every conversation`);
        await refreshMcp();
        return;
      }
    }
    setMessage(`${name}: still waiting on the browser — press a to start over`);
  }, [controls, refreshMcp]);

  /**
   * Page the FOCUSED diff, when there is one. Returns whether it consumed the key.
   *
   * Also what a wheel tick reaches (`scrollBy` on the handle): the wheel scrolled the
   * transcript underneath an open panel, so the one surface whose whole job is reading a
   * long diff was the one surface the wheel did not touch.
   */
  const pageDiff = (dir: -1 | 1): boolean => {
    if (!(panel.tab === "changes" && focusDiff)) return false;
    const page = Math.max(1, bodyRows - 2);
    setDiffScroll((i) => Math.max(0, i + dir * page));
    return true;
  };

  const confirm = useCallback((summarize = false, at = sel) => {
    // The URL buffer takes ⏎ before any tab does: while it is open the panel is
    // asking a question, and the row under the cursor is not what the key is about.
    if (filtering && entryKind === "mcpUrl") {
      const url = filter.trim();
      setFiltering(false);
      setFilter("");
      setEntryKind("filter");
      if (!url || url === "https://") return setMessage(null);
      const name = nameFromUrl(url, Object.keys(mcp?.registry.servers ?? {}));
      if (!name) return setMessage(`${url} is not a URL bough can name a server from`);
      if (!controls.putMcpServer) {
        return setMessage("registering an MCP server is not wired into this client");
      }
      setMessage(`registering ${name}…`);
      // Registering GRANTS NOTHING (`mcp/config.ts`) — the row appears "off" and ⏎
      // is what turns it on. Authorization is offered in the same breath because a
      // remote server that needs it is useless until it has it, but it is still the
      // user's keypress: `a`, named in the message, not started behind their back.
      void controls.putMcpServer(name, { url })
        .then(() => refreshMcp())
        .then(() => {
          setMessage(`registered ${name} — a authorizes it, ⏎ grants it in every conversation`);
        })
        .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
      return;
    }
    switch (panel.tab) {
      case "tree": {
        // ONE list, three kinds of row, and `selectionFor` decides which is which:
        // a conversation OPENS (the switcher half of this surface stays one key), a
        // collapsed fan-out DRILLS IN, and a turn FORKS — pi's selection rules, so a
        // user turn cuts BEFORE itself and hands its text to the composer while
        // anything else cuts inclusive and leaves the composer empty.
        const row = tree[at];
        if (!row) return;
        const choice = selectionFor(row, threads);
        if ("none" in choice) return; // a topic caption — nothing to open, fork or drill into
        if ("drill" in choice) return drillIn(choice.drill);
        if ("open" in choice) {
          dispatch({ type: "close" });
          return void store.open(choice.open);
        }
        if ("expand" in choice) return expand(choice.expand);
        dispatch({ type: "close" });
        // The fork is addressed to the row's OWN conversation, not to the open one:
        // the forest shows every conversation's turns, so ⏎ on a branch's turn must
        // branch that branch rather than whatever happens to be on screen.
        return void controls.forkAt?.(
          choice.fork.sessionId,
          {
            atMessageId: choice.fork.atMessageId,
            ...(choice.fork.exclusive ? { exclusive: true } : {}),
            ...(summarize ? { summarizeAbandoned: true } : {}),
          },
          choice.editorText,
        );
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
        // REVOKING ARMS FIRST (spec §7: consent is never inferred). Granting is
        // additive and stays one press; taking a server away from every conversation
        // is not, and it was reachable by ACCIDENT: the panel owns the keyboard while
        // it is open, so typing a message into what looks like the composer drops the
        // characters and delivers the Return here — where ⏎ toggled the row under the
        // cursor. Reproduced: with the mcp tab open, `hello there⏎` revoked
        // chrome-devtools install-wide and the only trace was a notice that expires.
        // Making the grant global (which is what it should always have been) is
        // exactly what turned that slip from one conversation into all of them.
        if (!on && armedStop !== `mcp:off:${name}`) {
          setArmedStop(`mcp:off:${name}`);
          return setMessage(
            `⏎ again to turn ${name} off in every conversation — it is granted now`,
          );
        }
        setArmedStop(null);
        // GLOBAL, not this conversation. A grant scoped to `state.currentId` lasted
        // exactly as long as the conversation it was made in, so every new one
        // started with every server off and the same server had to be granted again
        // and again — a setting wearing a per-conversation permission's clothes.
        // The scope has existed since the registry did (`""` in `mcp/config.ts`,
        // meaning every session) and the panel never used it.
        //
        // Session-scoped grants remain in the model and are still what a skill's
        // `mcp:` frontmatter and a TTL grant produce; this is about the verb a human
        // presses. A global revoke clears every scope server-side
        // (`revokeEverywhere`), so the message below is true of older conversations
        // too — grants made one at a time, which is how they were all made before.
        return void controls.setMcpEnabled(name, on, "")
          .then(() => {
            setMessage(
              on
                ? `${name} is granted in every conversation`
                : `${name} is off in every conversation`,
            );
            return controls.loadMcp?.(state.currentId ?? undefined).then(setMcp);
          })
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
    threads,
    expand,
    drillIn,
    controls.forkAt,
    // The URL buffer is read at the top of this callback, so a stale copy would
    // register whatever was typed the render before.
    filtering,
    entryKind,
    filter,
    refreshMcp,
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
   * `x` in the changes tab: arm a revert of the file under the cursor. Idempotent.
   *
   * A SECOND `x` USED TO WIDEN THE SCOPE TO EVERY CHANGED FILE, and that was a trap
   * built out of bough's own idioms. The rail's destructive key is `x x` — arm, then
   * confirm with the same key — so a user arriving here with that reflex pressed `x`
   * twice and landed on "revert all 3 files", one ⏎ from throwing away the whole
   * session's work, having asked for one file. The escalation was on screen, but the
   * gesture that produced it was the one they had been taught means "yes".
   *
   * Nothing is lost by removing it: `X` is bound to exactly that scope, says so in the
   * keymap and in the tab's own footer, and still needs its own ⏎.
   */
  const armRevert = useCallback(() => {
    if (changes.length === 0) return;
    if (pendingRevert) return;
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
    /**
     * `esc esc` — the tree, opened ON the turn you would go back to.
     *
     * Handled ahead of `panelActionFor` because the landing row is the entire
     * difference between this and `^f`: the open conversation is expanded (so its
     * turns are rows at all) and the cursor is put on its last user turn, where ⏎
     * means "edit this and branch". Arriving at the top of a forest of forty
     * conversations would make the commonest correction in the product a scroll.
     */
    if (command === "tree.rewind") {
      const id = state.currentId;
      if (!id) {
        setMessage("no conversation is open — there is no turn to go back to");
      }
      if (id) expand(id);
      setPanel({ open: true, tab: "tree" });
      // Against the rows as they will be WITH this conversation expanded: `expand`
      // is a state update and `tree` in this closure predates it, so the index is
      // computed from a forest built here rather than from the stale one.
      landOn.current = (
        rewindIndex(
          forestRows({
            ...deps.forest,
            currentId: id,
            expanded: id ? new Set([...deps.forest.expanded, id]) : deps.forest.expanded,
          }),
          id,
        )
      );
      // Set as well as parked: the reset effect only fires when the tab or the open
      // flag CHANGES, and `esc esc` inside an already-open tree changes neither.
      setSel(landOn.current);
      return true;
    }
    /**
     * `e` — SPLIT THE THREAD HERE. The turn under the cursor and every later turn of its
     * conversation become a fresh root, and it opens.
     *
     * `POST /sessions/:id/extract` has existed since the port with no key on it: the
     * server op, its schema and its tests all shipped, and nothing in the TUI ever
     * called it (found by grepping `api.ts` for methods with no caller). It answers the
     * case fork cannot — "this conversation turned into two pieces of work" — because
     * the new session is a ROOT that inherits no thread, so what it keeps is a
     * SELECTION rather than a prefix.
     *
     * Ahead of `panelActionFor` for the same reason `tree.rewind` is: it needs the row
     * under the cursor, which lives here and not in the dispatcher.
     *
     * Nothing is destroyed — the source keeps every turn (`history/extract.ts`) — so
     * this needs no arm-and-confirm the way `changes.revert` does.
     */
    if (command === "tree.extract") {
      const row = tree[sel];
      // Statements, not `return void f(), true` — `void` binds tighter than the comma, so
      // that form returns `undefined` and the key falls through to the dispatcher as
      // unhandled. Cost me a live run where `e` on a session row did nothing at all.
      if (!row || row.kind !== "message") {
        setMessage("e splits a conversation at a TURN — move onto one first");
        return true;
      }
      const thread = threads[row.sessionId] ?? [];
      const at = thread.findIndex((m) => m.id === row.id);
      if (at < 0) {
        setMessage("that turn is no longer in the thread");
        return true;
      }
      const picks = thread.slice(at).map((m) => ({ messageId: m.id }));
      if (!controls.extractFrom) {
        setMessage("extract is not wired into this client");
        return true;
      }
      dispatch({ type: "close" });
      void controls.extractFrom(row.sessionId, picks);
      return true;
    }
    /**
     * `m` — BRING THESE TURNS HERE. The other direction from `e`: the turn under the
     * cursor and every later turn of its conversation are copied onto the end of the
     * conversation that is OPEN.
     *
     * `POST /sessions/:id/move-into` was the second op in this pair with no key on it.
     * It is what you want when the context you need is in a conversation you already
     * abandoned — the alternative today is scrolling the tree and retyping it.
     *
     * The three unsound targets (itself, a session mid-turn, an ancestor of the source)
     * are refused BY THE SERVER with reasons — `history/move.ts` documents why each one
     * is unsound — and the refusal now lands in the tree's message row. Only the two
     * cases the server cannot see are checked here: no conversation open to receive
     * them, and the row being a turn at all.
     */
    if (command === "tree.moveInto") {
      const row = tree[sel];
      const target = state.currentId;
      if (!row || row.kind !== "message") {
        setMessage("m brings a TURN here — move onto one first");
        return true;
      }
      if (!target) {
        setMessage("no conversation is open to bring these turns into");
        return true;
      }
      if (row.sessionId === target) {
        // Said locally rather than as a 400: this is the likely slip, and "a session
        // cannot append its own turns to its own tail" reads as a fault when it is
        // really just the wrong row.
        setMessage("those turns are already in this conversation");
        return true;
      }
      const thread = threads[row.sessionId] ?? [];
      const at = thread.findIndex((m) => m.id === row.id);
      if (at < 0) {
        setMessage("that turn is no longer in the thread");
        return true;
      }
      if (!controls.moveIntoOpen) {
        setMessage("move-into is not wired into this client");
        return true;
      }
      dispatch({ type: "close" });
      void controls.moveIntoOpen(target, row.sessionId, thread.slice(at).map((m) => ({
        messageId: m.id,
      })));
      return true;
    }
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
        // → WALKS IN, one step at a time: a conversation shows its turns, a
        // collapsed fan-out reveals itself. On a turn there is nothing further in —
        // the branches under it are already rows — so it does nothing rather than
        // pretending.
        if (item.kind === "session") expand(item.id);
        else if (item.kind === "collapsed") drillIn(item.originId);
        return true;
      }
      case "move.out": {
        if (back()) return true;
        const item = tree[sel];
        if (panel.tab !== "tree" || !item) return true;
        // ← is →'s inverse on a conversation. On a TURN it closes the conversation
        // that turn belongs to, which is the only useful "out" from inside one and
        // saves walking back up to the header row to press ← there.
        if (item.kind === "session") {
          collapseTurns(item.id);
          collapse(item.id);
        } else if (item.kind === "message" || item.kind === "section") {
          // A section header belongs to a conversation the same way a turn does, so ← out of
          // one closes that conversation rather than doing nothing.
          collapseTurns(item.sessionId);
        } else collapse(item.originId);
        return true;
      }
      // ---- a screenful at a time (spec §3: a list you can only walk one row at a
      // time is a list whose far end does not exist — the model tab is 32 rows in a
      // ~20-row viewport). The page is the tab's OWN viewport, not a constant.
      // A SCREENFUL OF WHAT THE CURSOR IS IN. In focus mode ↑↓ scroll the diff and the
      // legend says so — but paging moved the FILE cursor, so `PageDown` while reading a
      // 121-line diff silently switched which file `x revert this path` would take, with the
      // legend unchanged. A reviewer persona found it by pressing the obvious key.
      case "move.pageUp":
        if (pageDiff(-1)) return true;
        moveTo(sel - Math.max(1, bodyRows - 2));
        return true;
      case "move.pageDown":
        if (pageDiff(1)) return true;
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
        setEntryKind("filter");
        setFiltering(true);
        return true;
      case "panel.filterBack":
        setFilter((f) => f.slice(0, -1));
        setSel(0);
        return true;
      case "panel.filterExit":
        // Esc unwinds ONE level: it drops the filter and keeps the panel open. The
        // keymap orders this ahead of `panel.close`, so the next esc closes.
        //
        // AND IT KEEPS THE ROW YOU FOUND. Dropping the filter reset the cursor to 0, so
        // searching "binary", landing on the one conversation that matched, and pressing
        // esc put you back at the top of a nine-row forest with the found row somewhere
        // in it — the search's whole result, discarded by the key that ends the search.
        // Parked by ID because the widened list renumbers every index.
        landOnId.current = tree[sel]?.id ?? null;
        setFiltering(false);
        setFilter("");
        setEntryKind("filter");
        setSel(0);
        return true;

      // `p`/`P`/`r`/`x` are the WORKFLOWS tab's verbs and used to reach the dispatcher
      // from every tab, steering `state.workflows[sel]` — the row a cursor in an
      // ---- the mcp tab's verbs ------------------------------------------------
      case "mcp.add":
        // The buffer only; the write happens on ⏎ (`confirm`). Prefilled with the
        // scheme because every remote MCP server is https and typing it is friction
        // on the one field this flow has.
        setEntryKind("mcpUrl");
        setFilter("https://");
        setFiltering(true);
        setMessage(null);
        return true;

      case "mcp.auth": {
        const name = mcpNameAt(sel);
        if (!name) return true;
        if (!controls.beginMcpAuth) {
          setMessage("authorizing an MCP server is not wired into this client");
          return true;
        }
        // ALREADY HAS ONE. Pressing `a` on a server carrying its own credential
        // starts a flow that answers a question nobody asked, and for a provider
        // without dynamic registration it ends in a wall of text about creating an
        // OAuth app — which is what you get for the ONE server that needed nothing.
        // Not a refusal: bringing your own authorization is legitimate, and `a`
        // again does it. What was missing is the sentence before the wall.
        if (hasStaticAuth(mcp?.registry.servers[name]) && armedStop !== `auth:${name}`) {
          setArmedStop(`auth:${name}`);
          setMessage(
            `${name} already has a credential (keychain) — press c to test it. ` +
              `a again starts a separate OAuth authorization anyway.`,
          );
          return true;
        }
        setArmedStop(null);
        setMessage(`authorizing ${name}…`);
        void controls.beginMcpAuth(name)
          .then((start) => {
            if (start.status === "authorized") {
              setMessage(`${name} is authorized`);
              return refreshMcp();
            }
            if (!start.authorizationUrl) {
              setMessage(`${name}: the server asked for authorization but sent no URL`);
              return;
            }
            // PRINTED, never opened. A headless server that shells out to a browser
            // hangs when there is no browser (`mcp/oauth.ts`), and the terminal makes
            // the URL clickable itself.
            setMessage(`open this, then come back — it finishes on its own: ${start.authorizationUrl}`);
            // The server may have CORRECTED the registry on the way through — the
            // published URL is often not the one the flow wants (`mcp/oauth.ts`) —
            // so the row must be re-read or it keeps showing the URL that failed.
            return refreshMcp().then(() => pollAuth(name));
          })
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
        return true;
      }

      /**
       * `c` — connect to this server NOW and say what happened.
       *
       * The tab was full of intentions and had no proof. "keychain" says which
       * credential will be tried, not that the endpoint accepts it, so the only way
       * to learn that a synced Slack grant actually works was to spend a turn on a
       * tool call and read the failure — or to press `a`, which starts an OAuth
       * flow the server may not even support and answers a question nobody asked.
       * Connecting is not granting (`mcp/status.ts`), so this changes no permission.
       */
      case "mcp.restart": {
        const name = mcpNameAt(sel);
        if (!name) return true;
        if (!controls.restartMcpServer) {
          setMessage("restarting an MCP server is not wired into this client");
          return true;
        }
        // Per-session by construction: what is being restarted is a subprocess spawned in
        // a conversation's checkout, so the server refuses a scopeless restart (400) and
        // saying so here is clearer than relaying that.
        if (!state.currentId) {
          setMessage(`open a conversation first — a server's process runs in its checkout`);
          return true;
        }
        setMessage(`restarting ${name}…`);
        void controls.restartMcpServer(name, state.currentId)
          .then((r) => {
            // `restarted: false` is not a failure: there was nothing running to replace,
            // and the next call will start one. Saying "restarted" there would claim an
            // action that did not happen.
            setMessage(
              r.restarted
                ? `${name} restarted — c tests it`
                : `${name} was not running · the next call starts it`,
            );
            return refreshMcp();
          })
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
        return true;
      }
      case "mcp.connect": {
        const name = mcpNameAt(sel);
        if (!name) return true;
        if (!controls.connectMcpServer) {
          setMessage("testing an MCP connection is not wired into this client");
          return true;
        }
        // No conversation needed for a remote server — its connection belongs to the
        // process (`mcp/service.ts`). A stdio server still does, because it is a
        // subprocess spawned in a conversation's checkout, and the server says so.
        setMessage(`connecting to ${name}…`);
        void controls.connectMcpServer(name, state.currentId ?? "")
          .then((r) => {
            const tools = r.tools ?? [];
            setMessage(
              r.connected
                ? `${name} connected · ${tools.length} tool${tools.length === 1 ? "" : "s"}` +
                  (tools.length > 0 ? ` · ${tools.slice(0, 6).map((t) => t.name).join(", ")}` : "")
                : `${name} did not connect — ${r.error ?? "no reason given"}`,
            );
            return refreshMcp();
          })
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
        return true;
      }
      // Deleting the REGISTRATION, armed then confirmed — the same idiom as the
      // rail's `x` and the workflows tab's, because it is the same kind of act.
      // `F` next door drops credentials and keeps the entry; these were one verb in
      // everyone's head and neither did the other's job, so each says which it is.
      case "mcp.remove": {
        const name = mcpNameAt(sel);
        if (!name) return true;
        if (!controls.deleteMcpServer) {
          setMessage("removing an MCP server is not wired into this client");
          return true;
        }
        if (armedStop !== `mcp:${name}`) {
          setArmedStop(`mcp:${name}`);
          // The scope, out loud: removing an entry also revokes the grants it
          // orphans (`mcp/config.ts`), and that is not obvious from "delete".
          setMessage(
            `d again to delete ${name} — its registration and any grants it holds. ` +
              `Stored credentials are dropped with it; the server itself is untouched.`,
          );
          return true;
        }
        setArmedStop(null);
        void controls.deleteMcpServer(name)
          .then(() => {
            setMessage(`deleted ${name}`);
            return refreshMcp();
          })
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
        return true;
      }
      case "mcp.forget": {
        const name = mcpNameAt(sel);
        if (!name) return true;
        if (!controls.clearMcpAuth) {
          setMessage("clearing MCP credentials is not wired into this client");
          return true;
        }
        // The scope is said out loud rather than implied: this drops the stored
        // tokens for ONE server and nothing else.
        void controls.clearMcpAuth(name)
          .then(() => {
            setMessage(`forgot ${name}'s credentials — press a to authorize again`);
            return refreshMcp();
          })
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
        return true;
      }

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
      case "wf.save": {
        // Saved under the run's own `meta.name`, with no name prompt: the name is
        // already the thing the author chose to call it, and the route is idempotent
        // on it, so saving twice updates rather than accumulating `audit-2`.
        if (!wfOpen || !wfDetail) {
          return void setMessage("open a run first — s saves that run's script"), true;
        }
        const name = wfDetail.workflow.name;
        setMessage(null);
        void api.saveWorkflowAs(wfOpen, name)
          // HOW to run it, not just that it can be. Nothing in the TUI runs a saved
          // workflow — `api.runSavedWorkflow` and `listSavedWorkflows` are never called — so
          // "it can be run again by name" was a promise the product could not keep on its own.
          // The agent can (`workflow.start({name})`), and `/saved` lists what exists.
          .then(() =>
            setMessage(`saved as "${name}" — ask the agent to run it by name · /saved lists them`)
          )
          .catch((e: unknown) => setMessage(e instanceof Error ? e.message : String(e)));
        return true;
      }
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
        mcp={{
          status: mcp,
          selected: sel,
          message,
          rows: body,
          // The URL being typed. `null` when the buffer is closed or is a filter for
          // some other tab — the two share one buffer, not one meaning.
          entry: filtering && entryKind === "mcpUrl" ? filter : null,
        }}
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
            // `bodyRows`, not `body`: this tab arrives as `children`, so `Panel`
            // cannot subtract its own chrome for it the way it does for the tabs it
            // mounts itself. It was handed two rows it did not have, every time.
            <Tree
              rows={tree}
              selected={sel}
              height={bodyRows}
              filter={filter}
              filtering={filtering}
              message={message}
              workspace={state.sessions.find((s) => s.id === state.currentId)?.workspace ?? null}
              // Inside the panel's border and padding, like every other tab's legend.
              cols={Math.max(20, cols - 4)}
            />
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
                // Minus the border and padding, like the tree beside it: measured against
                // the panel's full width, this tab's footer lost its last character and
                // ended `· esc` instead of `· esc back`.
                cols={Math.max(20, cols - 4)}
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
    /**
     * A wheel tick, offered to the panel first. Returns whether the panel took it.
     *
     * Only the focused diff takes one today: a wheel tick over a LIST would move the cursor,
     * which changes what a revert key targets — a scroll gesture must never do that.
     */
    scrollBy: (rows: number) => {
      if (!(panel.open && panel.tab === "changes" && focusDiff)) return false;
      setDiffScroll((i) => Math.max(0, i + rows));
      return true;
    },
    filtering,
    filterInput,
    openRun,
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
