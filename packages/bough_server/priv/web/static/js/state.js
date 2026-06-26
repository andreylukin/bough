export const ACTIVE = new Set(["running", "awaiting_plan", "awaiting_net", "awaiting_group", "awaiting_capability"]);

export const state = {
  config: { provider: "", model: "" },
  models: { default: { provider: "", model: "" }, providers: [] }, // /models picker catalog
  sessions: [],
  sessionId: null,
  tree: null,            // { id, project, active_leaf, entries[], superseded[], groups[], suggested[] }
  run: null,             // { status, steps[], text, context_tokens, network[] }
  subagents: [],
  showDoneSubs: false,   // Subagents pane: reveal the collapsed completed ones
  capsOpen: {},          // Capabilities pane: per-subsection collapse state (key → bool)
  capsFilter: "",        // Capabilities pane: search query over groups
  diff: null,            // { sessionId, git, files[], patch } — lazy-loaded Changes tab
  files: null,           // { sessionId, list[] } — workspace files for the "@" picker
  pastes: [],            // large clipboard pastes collapsed into chips, expanded on send
  groupsCatalog: [],
  packs: [],
  rightTab: "tree",
  reviewArmed: false,
  graftRoot: null,       // node id selected as a graft section root
  mapOpen: false,        // session map overlay (pannable/zoomable 2-D tree)
  mapView: null,         // map camera { x, y, scale }; null = fit on next paint
  mapShowSuperseded: false,
  mapExpanded: new Set(), // map: turn ids whose tool-call steps are expanded
  viewChildId: null,     // when set, the transcript shows this subagent
  childTree: null,
  childRun: null,
  poll: null,
  filter: "",            // sidebar search query
  openProjects: null,    // sidebar: which project groups are expanded (null = default)
  lastFocus: null,       // focus to restore when the drawer closes
  paneSig: null,         // last rendered per-pane signatures (skip no-op re-renders)
};

// ---- API -----------------------------------------------------------------
