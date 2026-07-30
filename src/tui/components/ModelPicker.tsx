/**
 * The model tab: both tiers, one list.
 *
 * THE INVARIANT THIS HOLDS: **the picker chooses the frontier model AND the cheap
 * model.** Spec §12 names two tiers and says both are chosen here — the supervisor,
 * and the cheap model that powers auto titles, composer ghost text and live activity
 * blurbs. A picker that offered only the frontier tier would leave the tier that bills
 * on *every* round unreachable from the product, configurable only by editing state
 * the user cannot see. So `modelEntries` emits two model sections over the same
 * catalog, and `chooseEntry` routes the choice by the row's `tier` and nothing else.
 *
 * SECOND INVARIANT — **switching pins THIS session and moves the default for new
 * sessions, and touches no other existing session.** That sentence is spec §12
 * verbatim, and it is implemented as one pure function (`chooseEntry`) rather than as
 * two API calls a caller might make only one of. It writes `sessionModel` (the pin)
 * and `defaultModel` (what new sessions inherit); every other session keeps whatever
 * it was already pinned to, because nothing here can express a change to them. The
 * cheap tier has no per-session pin — it is one background model for the whole
 * install, so choosing one moves the default only.
 *
 * THIRD — **an id is a provider routing decision, so the catalog is injected.** Model
 * ids route by prefix (`openai:x` → OpenAI, `vendor/model` → OpenRouter, bare →
 * Anthropic — spec §12), and that table lives in `llm/client.ts` where the routing
 * does. This file takes `ModelRow[]` as a prop and imports the type only, so no
 * provider name is written outside `llm/`, no provider SDK is dragged into the TUI's
 * import graph, and a test drives the picker with three fixture rows.
 *
 * NOT PORTED: the API-keys section of `src/tui/components/ModelPicker.tsx`. Keys are
 * environment variables in this tree (`API_KEY_ENV` in `llm/client.ts`) and there is
 * no `/config/keys` route to write to, so a "paste key" row would be a control that
 * does nothing. When a keys route lands it belongs here as a fourth section.
 *
 * KNOWN GAP: there is no `/config` route yet either (`GET/PATCH /config` is not in
 * `server/app.ts` and `api.ts` has no method for it). This component is therefore
 * presentational over a `ModelConfig` the caller owns, and `chooseEntry` returns the
 * next config rather than performing a write. Reported rather than worked around.
 */
import { TextAttributes } from "@opentui/core";
import type { ModelRow } from "../../llm/client.ts";
import type { Effort } from "../../types.ts";
import { clip, legendLine, windowAround } from "../format.ts";
import { palette } from "../theme.ts";

// ---------------------------------------------------------------------------
// The config the picker edits
// ---------------------------------------------------------------------------

/** `"default"` leaves the request untouched — the provider decides. */
export type EffortChoice = "default" | Effort;

export const EFFORTS: readonly EffortChoice[] = [
  "default",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

const EFFORT_LABELS: Record<EffortChoice, string> = {
  default: "adaptive — the provider decides",
  low: "low — quick, minimal thinking",
  medium: "medium — balanced",
  high: "high — thorough",
  xhigh: "xhigh — deep (the agentic sweet spot)",
  max: "max — correctness over cost",
};

/**
 * A stored effort string narrowed to a row this picker can mark.
 *
 * The session row types `effort` as a free `string | null` (it is a column, and the
 * server accepts whatever a future model names), while the picker's sections are the
 * fixed `EFFORTS` list. Anything unrecognised reads as "no row of mine" rather than as
 * a row that does not exist — the same rule the frontier section already follows.
 */
export function asEffortChoice(value: string | null | undefined): EffortChoice | null {
  return EFFORTS.includes(value as EffortChoice) ? (value as EffortChoice) : null;
}

export interface ModelConfig {
  /** What a NEW session starts on. */
  defaultModel: string;
  /** THIS session's pin. `null` = it follows `defaultModel`. */
  sessionModel: string | null;
  /** The cheap tier: titles, ghost text, activity blurbs. One per install. */
  cheapModel: string | null;
  defaultEffort: EffortChoice;
  sessionEffort: EffortChoice | null;
}

/** What the open session actually runs on right now. */
export function effectiveModel(cfg: ModelConfig): string {
  return cfg.sessionModel ?? cfg.defaultModel;
}

export function effectiveEffort(cfg: ModelConfig): EffortChoice {
  return cfg.sessionEffort ?? cfg.defaultEffort;
}

// ---------------------------------------------------------------------------
// Entries (pure)
// ---------------------------------------------------------------------------

export type Tier = "frontier" | "cheap" | "effort";

interface EntryBase {
  label: string;
  /** Right-hand detail: the raw id, and which provider the prefix routes to. */
  detail: string;
}

/**
 * Discriminated on `tier` so an effort row's id is an `EffortChoice` and a model row's
 * is a free-form model id — the two are not interchangeable, and a flat `id: string`
 * let `chooseEntry` write "xhigh" into `defaultModel` without a complaint.
 */
export type ModelEntry =
  | (EntryBase & { tier: "frontier" | "cheap"; id: string })
  | (EntryBase & { tier: "effort"; id: EffortChoice });

export const SECTIONS: Record<Tier, { title: string; hint: string }> = {
  frontier: {
    title: "frontier model — the supervisor",
    // Kept under 70 characters ON PURPOSE: the hint is indented two columns inside the
    // panel border, and 80 columns is the narrowest terminal bough claims to support.
    hint: "pins this session, and new ones; others keep what they have",
  },
  cheap: {
    title: "cheap model — titles, ghost text, activity",
    hint: "bills on every round, so it fails silently and never blocks a turn",
  },
  effort: { title: "thinking depth", hint: "not every model accepts one" },
};

/** The flat entry list: frontier catalog, cheap catalog, then the effort levels. */
export function modelEntries(
  catalog: readonly ModelRow[],
  cheapCatalog: readonly ModelRow[] = catalog,
): ModelEntry[] {
  return [
    ...catalog.map((m) => row("frontier", m)),
    ...cheapCatalog.map((m) => row("cheap", m)),
    ...EFFORTS.map((e) => ({
      tier: "effort" as const,
      id: e,
      label: EFFORT_LABELS[e],
      detail: e,
    })),
  ];
}

function row(tier: "frontier" | "cheap", m: ModelRow): ModelEntry {
  return { tier, id: m.id, label: m.label, detail: `${m.id}  ·  ${m.provider}` };
}

/** Whether an entry is the one currently in force for its tier. */
export function isActive(cfg: ModelConfig, e: ModelEntry): boolean {
  switch (e.tier) {
    case "frontier":
      return effectiveModel(cfg) === e.id;
    case "cheap":
      return cfg.cheapModel === e.id;
    case "effort":
      return effectiveEffort(cfg) === e.id;
  }
}

/**
 * Choosing a row. **This is spec §12 in code**: a frontier pick pins the open session
 * and moves the default for new sessions; nothing else moves. Pure — the caller sends
 * the resulting config to the server and re-renders from the response.
 */
export function chooseEntry(cfg: ModelConfig, e: ModelEntry): ModelConfig {
  switch (e.tier) {
    case "frontier":
      return { ...cfg, sessionModel: e.id, defaultModel: e.id };
    case "cheap":
      // No per-session pin: one background model for the whole install.
      return { ...cfg, cheapModel: e.id };
    case "effort":
      return { ...cfg, sessionEffort: e.id, defaultEffort: e.id };
  }
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

/**
 * What the cheap section says when nothing in it is marked.
 *
 * The ● means "this is what runs". Every other section has one, and the cheap section
 * had none whenever `cheapModel` was unset — so the tab that exists to answer "which
 * model is selected" answered it for two tiers out of three, and the absence read as a
 * missing dot rather than as a state. Unset is a real state and it gets a real row.
 *
 * It does not NAME a model, because when this row shows there is none to name. The
 * cheap tier is resolved server-side (`BOUGH_CHEAP_MODEL`, `worker/titles.ts`) and
 * `GET /model-settings` now reports it alongside the frontier default, so the normal
 * case marks the real row with a ●. This row is what is left: the settings fetch has
 * not answered yet, or the install genuinely has no cheap tier. Naming a guess here is
 * exactly the bug the frontier section already had and had fixed.
 */
export const CHEAP_UNSET =
  "(unset) — no cheap model is known for this install; pick a row to set one";

export type DisplayRow =
  | { header: Tier }
  /** A section's explanation, on its own row so the window height counts it. */
  | { hint: string }
  | { entry: ModelEntry; index: number }
  | { note: string };

/**
 * Section headers interleaved with entries — what the cursor window is cut from.
 *
 * `cheapUnset` inserts the state row under the cheap header. It goes through here and
 * not into the component's JSX so the window height still counts every row it paints;
 * a row rendered outside the count is a row the renderer clips into garbage.
 */
export function displayRows(
  entries: readonly ModelEntry[],
  opts: { cheapUnset?: boolean } = {},
): DisplayRow[] {
  const out: DisplayRow[] = [];
  let last: Tier | null = null;
  entries.forEach((entry, index) => {
    if (entry.tier !== last) {
      out.push({ header: entry.tier });
      out.push({ hint: SECTIONS[entry.tier].hint });
      if (entry.tier === "cheap" && opts.cheapUnset) out.push({ note: CHEAP_UNSET });
    }
    last = entry.tier;
    out.push({ entry, index });
  });
  return out;
}

export interface ModelPickerProps {
  /** Columns available, so the legend degrades instead of being cut mid-word. */
  cols?: number;
  cfg: ModelConfig;
  entries: ModelEntry[];
  selected: number;
  rows: number;
  message?: string | null;
  /** The `/` filter buffer. Narrowing happens in `PanelHost`; this only draws it. */
  filter?: string;
  filtering?: boolean;
}

/**
 * The visible slice of the interleaved header/entry list.
 *
 * Sized from what is ACTUALLY left after the chrome. `Math.max(3, rows - 6)` claimed
 * three rows it did not have below nine, and the overflow did not scroll — it merged
 * rows into each other (`Panel.tsx`). Everything countable is counted: the message,
 * the filter line, the legend, and the two `↑ n more` / `↓ n more` markers.
 *
 * Exported, like `sessionsWindow`, so `PanelHost` can resolve a digit against the
 * rows that are actually on screen rather than against a second guess at them.
 */
export function modelWindow(
  display: readonly DisplayRow[],
  selected: number,
  rows: number,
  chrome = 0,
): { start: number; end: number; height: number; marks: boolean } {
  const avail = Math.max(0, rows - chrome - 1 /* legend */);
  // ONE row for both markers, not two. As a pair they cost two rows, and when only
  // two were left they cost ALL of them — the tab said "↑ 1 more / ↓ 35 more" above a
  // list of nothing. Content wins when it is tight; the legend never gives up its row.
  const marks = display.length > avail && avail >= 3;
  const height = Math.max(0, avail - (marks ? 1 : 0));
  const cursorAt = Math.max(0, display.findIndex((d) => "entry" in d && d.index === selected));
  const { start, end } = windowAround(cursorAt, display.length, height);
  return { start: Math.max(0, start), end, height, marks };
}

/**
 * Entry indices in the window, top to bottom — exactly what `1`–`9` address.
 *
 * Headers and the `(unset)` note are NOT numbered: a digit that lands on a section
 * title is a digit that does nothing, and spec §3 wants the options addressable, not
 * the decoration between them.
 */
export function visibleEntries(display: readonly DisplayRow[], start: number, end: number) {
  return display.slice(start, end).flatMap((d) => ("entry" in d ? [d.index] : []));
}

export function ModelPicker(
  { cfg, entries, selected, rows, message, filter = "", filtering = false, cols }:
    ModelPickerProps,
) {
  const display = displayRows(entries, { cheapUnset: cfg.cheapModel === null });
  const chrome = (message ? 1 : 0) + (filtering || filter ? 1 : 0);
  const { start, end, height, marks } = modelWindow(display, selected, rows, chrome);
  // The entry ordinal within the window, so the digits run 1,2,3… down the entries
  // even where a section header sits between two of them.
  let ordinal = 0;
  return (
    <box flexDirection="column">
      {message
        ? <text fg={palette.warn} wrapMode="none">{message}</text>
        : null}
      {filtering
        ? (
          <text>
            <span fg={palette.accent}>{"/ "}</span>
            {filter}
            <span fg="black" bg={palette.accent}>{" "}</span>
          </text>
        )
        : filter
        ? <text attributes={TextAttributes.DIM}>/ {filter}</text>
        : null}
      {height > 0 && display.length === 0
        ? <text attributes={TextAttributes.DIM}>nothing matches that filter</text>
        : null}

      {(height === 0 ? [] : display.slice(start, end)).map((d) => {
        if ("hint" in d) {
          return (
            <text key={`hint-${d.hint.slice(0, 12)}`} attributes={TextAttributes.DIM} wrapMode="none">
              {`  ${d.hint}`}
            </text>
          );
        }
        if ("note" in d) {
          return (
            <text key="cheap-unset" wrapMode="none">
              <span>{"    "}</span>
              <span fg={palette.accent}>●</span>
              <span attributes={TextAttributes.DIM}>{" "}{clip(d.note, 88)}</span>
            </text>
          );
        }
        if ("header" in d) {
          const section = SECTIONS[d.header];
          // The TITLE only. The hint is a `hint` row of its own (`displayRows`), because
          // sharing one row put a 76-character sentence after a 32-character heading and
          // `wrapMode="none"` cut it at the panel border: at 120 columns the frontier
          // hint read `…other sessions are left alo`. It could not simply be rendered as
          // a second row here — the window height counts DisplayRows, and a row painted
          // outside that count is a row the renderer clips into garbage.
          return (
            <text key={`h-${d.header}`} wrapMode="none">
              <span attributes={TextAttributes.BOLD} fg={palette.accent}>{section.title}</span>
            </text>
          );
        }
        const { entry, index } = d;
        const sel = index === selected;
        const active = isActive(cfg, entry);
        ordinal += 1;
        return (
          <text key={`${entry.tier}:${entry.id}`} wrapMode="none">
            {/* The digit that selects this row, printed on it — spec §3 wants a
                NUMBERED LIST, not a shortcut you have to be told about. It counts
                entries and skips headers, which is the same thing `visibleEntries`
                counts, so what is printed is what `panel.pick` resolves. */}
            <span attributes={TextAttributes.DIM}>{ordinal <= 9 ? `${ordinal} ` : "  "}</span>
            <span fg={sel ? palette.accent : undefined}>{sel ? "❯ " : "  "}</span>
            <span fg={sel || !active ? undefined : palette.accent}>{active ? "●" : " "}</span>
            <span attributes={sel ? TextAttributes.BOLD : TextAttributes.NONE}>
              {" "}{clip(entry.label, 38)}
            </span>
            <span attributes={TextAttributes.DIM}>{"  "}{clip(entry.detail, 34)}</span>
          </text>
        );
      })}
      {marks
        ? (
          <text attributes={TextAttributes.DIM}>
            {start > 0 ? `↑ ${start}` : ""}
            {start > 0 && end < display.length ? " · " : ""}
            {end < display.length ? `↓ ${display.length - end}` : ""} more
          </text>
        )
        : null}
      {/* The legend is the LAST row, on every tab, naming only bound keys. */}
      <text attributes={TextAttributes.DIM} wrapMode="none">
        {filtering
          ? "type to narrow · ⌫ back · esc clear the filter · ↑↓ move · ⏎ choose"
          : "↑↓ move · pgup/pgdn page · 1-9 pick · / filter · ⏎ choose · esc back"}
      </text>
    </box>
  );
}
