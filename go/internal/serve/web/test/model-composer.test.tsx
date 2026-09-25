import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { ComposerHint, Thread, uploads } from "../src/app";
import { MentionsView } from "../src/mention";
import { SkillPickerView } from "../src/skills";
import { SelectView, type Option } from "../src/select";
import type { Row } from "../src/types";

// go/tests/model/specs/ui_composer.fizz at the component level: every
// settled state of the checked-in graph is rendered from props and
// storage built from that node, and the node's view fields (shown,
// send_label, send_enabled, queue_shown, stop_shown, edit_enabled,
// status_word, hint, composer_on) and invariants are asserted on the
// markup. The browser walk (tests/web/specs/model/ui_composer.spec.ts)
// drives the same graph through a real serve.
//
// The Thread is rendered whole: its draft, queue and failure rows come
// from the per-session storage it reads on mount, a send in flight from
// `sending`, an upload in flight from the module's uploads. A popover's
// open state and list live in the popover's own hooks, which a static
// render never runs, so an open popover is its View rendered from the
// node beside the Thread; "loading" is the panel past its 200 ms (the
// spec's loading is "blank or Loading…", and the settled screen says it).

type Node = {
  session: "a" | "b"; status: "idle" | "running" | "archived"; pending: boolean; queued: number; failed: boolean;
  draft_a: string; draft_b: string; popover: string; listing: string; focus: string; placeholder_sent: boolean;
  shown: string; composer_on: boolean; send_label: string; send_enabled: boolean; queue_shown: boolean;
  stop_shown: boolean; edit_enabled: boolean; status_word: string; hint: string;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_composer", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Composer#0.")) n[k.slice(11)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

const ID = { a: "mc-a", b: "mc-b" } as const;
const PASTE = "line\n".repeat(20);
// The draft's text per spec value, and the attachments stored beside it.
const DRAFT: Record<string, { text: string; atts?: { pastes: string[]; images: string[] } }> = {
  "": { text: "" },
  typed: { text: "fix the tests" },
  multi: { text: "fix the tests\nand the docs" },
  pasted: { text: "[Pasted text #1 +20 lines] ", atts: { pastes: [PASTE], images: [] } },
  // The slot is "" until the upload answers: in flight, or lost when nothing is uploading it.
  uploading: { text: "[Image #1] ", atts: { pastes: [], images: [""] } },
  lost: { text: "[Image #1] ", atts: { pastes: [], images: [""] } },
};
const QUEUED = "then run vet";
const FAILED = "deploy it";

const noop = () => {};
const props = { onSend: async () => null, onAnswer: async () => null, onInterrupt: noop, onArchive: noop, onRename: async () => {}, onModel: noop,
  onEffort: noop, onAssign: noop, onBack: noop, onContext: noop, onAck: noop, projects: [], busy: false, jump: null, lines: [] };

class Store {
  m = new Map<string, string>();
  getItem(k: string) { return this.m.get(k) ?? null; }
  setItem(k: string, v: string) { this.m.set(k, v); }
  removeItem(k: string) { this.m.delete(k); }
}

/** The Thread on screen at a node, rendered over the storage and uploads that node implies. */
function renderThread(n: Node): string {
  const id = ID[n.session];
  const local = new Store(), session = new Store();
  for (const [s, d] of [["a", n.draft_a], ["b", n.draft_b]] as const) {
    const { text, atts } = DRAFT[d];
    if (text) local.setItem("bough:draft:" + ID[s], text);
    if (atts) local.setItem("bough:draft-atts:" + ID[s], JSON.stringify(atts));
  }
  if (n.queued) session.setItem("bough:queue:" + ID.a, JSON.stringify([{ id: "q1", text: QUEUED }]));
  if (n.failed) session.setItem("bough:failed:" + ID.a, JSON.stringify([{ id: "f1", at: 0, text: FAILED, answer: false, error: "503" }]));
  const inflight = n.draft_a === "uploading" ? new Set([{ slot: 0, tag: "Image #1", done: new Promise<string>(noop) }]) : null;
  if (inflight) uploads.set(ID.a, inflight);
  const onA = n.session === "a";
  const row = { id, cwd: "/tmp/x", status: onA && n.status === "running" ? "running" : "idle", archived: onA && n.status === "archived" } as unknown as Row;
  const sending = onA && n.pending ? [{ id: "p1", text: "run it", after: 0, steer: false, at: "2026-09-24T12:00:00Z" }] : [];
  const g = globalThis as { localStorage?: unknown; sessionStorage?: unknown };
  const prev = [g.localStorage, g.sessionStorage];
  g.localStorage = local; g.sessionStorage = session;
  try { return renderToStaticMarkup(<Thread row={row} sending={sending} {...props} />); }
  finally { [g.localStorage, g.sessionStorage] = prev; if (inflight) uploads.delete(ID.a); }
}

const count = (html: string, re: RegExp) => (html.match(new RegExp(re.source, "g")) ?? []).length;
/** The markup of one element, found by an opening-tag pattern, through its matching close. */
function element(html: string, open: RegExp): string {
  const m = open.exec(html);
  if (!m) return "";
  const tag = /^<(\w+)/.exec(m[0])![1];
  let depth = 0;
  const re = new RegExp(`<(/?)${tag}\\b[^>]*?(/?)>`, "g");
  re.lastIndex = m.index;
  for (let t = re.exec(html); t; t = re.exec(html)) {
    if (t[2]) continue;
    depth += t[1] ? -1 : 1;
    if (depth === 0) return html.slice(m.index, re.lastIndex);
  }
  return html.slice(m.index);
}
const unescape = (s: string) => s.replace(/&quot;/g, '"').replace(/&#x27;/g, "'").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");
const disabled = (tag: string) => /\sdisabled=""/.test(tag);

/** The hint words the key hints show: the node's hint, off the markup. */
function hintOf(html: string): string {
  const hint = element(html, /<span class="hint composer-hint">/);
  if (!hint) return "hidden";
  const words = [...hint.matchAll(/<\/kbd>(?:<\/span>)? ([a-z]+)<\/span>/g)].map((m) => m[1]);
  if (words.includes("steer")) { expect(words).toEqual(["steer", "queue", "newline", "stop"]); return "steer"; }
  if (words.includes("stop")) { expect(words).toEqual(["send", "newline", "stop", "commands"]); return "live"; }
  expect(words).toEqual(["send", "newline", "commands", "files"]);
  return "send";
}

function checkThread(n: Node, html: string) {
  const area = /<textarea id="composer"([^>]*)>([^<]*)<\/textarea>/.exec(html)!;
  expect(area).not.toBeNull();
  // DraftIsTheSessions: the textarea holds the open session's own draft.
  expect(unescape(area[2])).toBe(DRAFT[n.shown].text);
  expect(n.shown).toBe(n.session === "a" ? n.draft_a : n.draft_b);
  // ArchivedIsReadOnly: a read-only composer cannot take focus or open a caret picker.
  expect(disabled(area[1])).toBe(!n.composer_on);
  expect(area[1].includes('placeholder="Read-only"')).toBe(!n.composer_on);
  expect(html.includes("archived-note")).toBe(!n.composer_on);
  if (!n.composer_on) {
    expect(n.focus).not.toBe("composer");
    expect(["slash", "mention"]).not.toContain(n.popover);
  }

  // SteerWhileRunning and NoControlThatWouldFail: the primary button.
  const send = /<button class="btn btn-primary"[^>]*>/.exec(html)![0];
  expect(send).toContain(`aria-label="${n.send_label}"`);
  expect(!disabled(send)).toBe(n.send_enabled);

  const queue = /<button class="btn composer-queue"[^>]*>/.exec(html)?.[0];
  expect(Boolean(queue)).toBe(n.queue_shown);
  // Queue waits for an upload, as Enqueue does.
  if (queue) expect(disabled(queue)).toBe(n.shown === "uploading");

  // LiveTurnHasStatus: Stop and the foot's word, exactly while A's turn is live.
  expect(html.includes('class="btn btn-ghost composer-stop"')).toBe(n.stop_shown);
  const word = /<span class="composer-status composer-status-(\w+)">/.exec(html)?.[1] ?? "";
  expect(word).toBe({ "": "", sending: "sending", running: "waiting" }[n.status_word]!);

  // HintsHiddenOnlyForPicker: a closed picker leaves the hints; the Thread's own picker is closed here.
  if (n.hint !== "hidden") expect(hintOf(html)).toBe(n.hint);

  // The attachment line: an upload in flight, or a tag whose file never arrived.
  expect(html.includes("Attaching image…")).toBe(n.shown === "uploading");
  expect(html.includes("Attachment unavailable: remove [Image #1]")).toBe(n.shown === "lost");

  // The failure row: its Edit only over an empty draft (EditOnlyOverEmpty).
  const failed = element(html, /<div class="send-failed composer-note composer-note-err" role="alert">/);
  expect(Boolean(failed)).toBe(n.session === "a" && n.failed);
  expect(count(html, /class="send-failed composer-note/)).toBe(n.session === "a" && n.failed ? 1 : 0);
  if (failed) {
    expect(failed).toContain("Not sent");
    expect(failed).toContain(FAILED);
    expect(failed).toMatch(/<button class="btn"[^>]*>Retry<\/button>/);
    expect(failed).toContain(">Discard</button>");
    const edit = /<button class="btn composer-edit"[^>]*>/.exec(failed)![0];
    expect(!disabled(edit)).toBe(n.edit_enabled);
  }
  if (n.edit_enabled) expect(n.failed && n.shown === "").toBe(true);

  // The queue: one row per queued message, and never a lost tag's placeholder (NoPlaceholderSent).
  const queued = element(html, /<ol class="queued"[^>]*>/);
  expect(count(queued, /<li class="queued-row">/)).toBe(n.session === "a" ? n.queued : 0);
  if (queued) expect(queued).not.toContain("[Image #1]");
  expect(n.placeholder_sent).toBe(false);

  // The toolbar is offered on every node, archived included (the spec's ClickSkills).
  const skills = /<button class="btn skills-btn"[^>]*>/.exec(html)![0];
  expect(disabled(skills)).toBe(false);
  expect(skills).toContain('aria-expanded="false"');
}

const SKILLS = [{ name: "grill-me", summary: "Question the plan" }, { name: "plan", summary: "Write a plan" }];

function checkCaretPicker(n: Node) {
  const kind = n.popover === "slash" ? "/" : "@";
  const files = kind === "@";
  const hits = n.listing === "list" ? (files ? [{ value: "go/main.go", label: "go/main.go" }] : SKILLS.map((s) => ({ value: s.name, label: "/" + s.name, hint: s.summary }))) : [];
  const html = renderToStaticMarkup(<MentionsView trigger={{ kind, token: "", from: 0, to: 1 }} hits={hits} at={0}
    failed={n.listing === "error"} loaded={n.listing === "list"} slow noSkills={false}
    onRetry={noop} onClose={noop} onPick={noop} onHover={noop} />);
  // The panel says something in every settled listing: the hints step aside for it.
  expect(html).toContain('class="mention"');
  // FocusFollowsPopover: nothing in the picker takes focus from the composer.
  expect(n.focus).toBe("composer");
  expect(count(html, /tabindex="-1"/)).toBe(hits.length);
  switch (n.listing) {
    case "loading":
      expect(html).toContain(files ? "Finding files…" : "Loading skills…");
      expect(html).not.toContain("listbox");
      break;
    case "error":
      expect(html).toContain(`Couldn’t load ${files ? "files" : "skills"}.`);
      expect(html).toMatch(/<button class="link">Retry<\/button>/);
      break;
    case "list":
      expect(html).toContain('role="listbox"');
      expect(count(html, /role="option"/)).toBe(hits.length);
      break;
    default:
      throw new Error(`unknown listing ${n.listing}`);
  }
  expect(renderToStaticMarkup(<ComposerHint pickerOpen steering={n.hint === "steer"} live={n.stop_shown} />)).toBe("");
}

function checkSkills(n: Node) {
  const all = n.listing === "list" ? SKILLS.map((s) => ({ ...s, manual: false })) : null;
  const html = renderToStaticMarkup(<SkillPickerView open disabled={false} hits={all ?? []} active={all ? 0 : -1} error={n.listing === "error"}
    all={all} slow q="" onToggle={noop} onClose={noop} onLeave={noop} onQ={noop} onKeys={noop} onHover={noop} onPick={noop} onRetry={noop} />);
  expect(html).toContain('aria-expanded="true"');
  // FocusFollowsPopover: the dialog's filter holds the keys.
  expect(n.focus).toBe("popover");
  const pop = element(html, /<div class="skills-pop" role="dialog"[^>]*>/);
  expect(pop).toContain('<input class="skills-filter"');
  switch (n.listing) {
    case "loading":
      expect(pop).toContain("Loading skills…");
      expect(pop).not.toContain('role="listbox"');
      break;
    case "error":
      expect(pop).toContain("Couldn’t load skills.");
      expect(pop).toMatch(/<button class="link">Retry<\/button>/);
      break;
    case "list":
      expect(pop).toContain('role="listbox"');
      expect(count(pop, /role="option"/)).toBe(SKILLS.length);
      break;
    default:
      throw new Error(`unknown listing ${n.listing}`);
  }
}

function checkSetting(n: Node) {
  const model = n.popover === "model";
  const options: Option[] = model ? [{ value: "m/one", label: "m/one" }, { value: "m/two", label: "m/two" }] : [{ value: "low", label: "Low" }, { value: "high", label: "High" }];
  const label = model ? "Next turn model" : "Next turn effort";
  // A pick the server refused: the Select stays open and says so beside the choice.
  const save = n.listing === "error" ? { value: options[1].value, state: "failed" as const } : null;
  // PickSetting refuses only on an archived session.
  if (save) expect(n.status).toBe("archived");
  const html = renderToStaticMarkup(<SelectView open save={save} shown={options} at={0} value={options[0].value} current={options[0]}
    listId="sel-list" optId={(i) => `sel-opt-${i}`} label={label} placeholder="Choose" searchable={model} align="end" disabled={false}
    pos={{}} q="" onKey={noop} onButton={noop} onRetrySave={noop} onQ={noop} onHover={noop} onPick={noop} />);
  // FocusFollowsPopover: focus is in the Select (its search field, or its combobox with the active option).
  expect(n.focus).toBe("popover");
  expect(html).toContain('aria-expanded="true"');
  expect(html).toContain(`aria-label="${label}: ${options[0].label}"`);
  if (model) expect(html).toContain('class="sel-search"');
  else expect(html).toContain('aria-activedescendant="sel-opt-0"');
  expect(count(html, /role="option"/)).toBe(options.length);
  expect(html.includes("Couldn’t save · Retry")).toBe(n.listing === "error");
  expect(n.listing).not.toBe("loading");
}

describe("ui_composer.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(100);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      checkThread(n, renderThread(n));
      // FocusFollowsPopover with nothing open: no popover holds focus.
      if (n.popover === "none") {
        expect(n.focus).not.toBe("popover");
        expect(n.hint).not.toBe("hidden");
        expect(n.listing).toBe("list");
      }
      if (n.popover === "slash" || n.popover === "mention") {
        expect(n.hint).toBe("hidden");
        checkCaretPicker(n);
      } else {
        expect(n.hint).not.toBe("hidden");
      }
      if (n.popover === "skills") checkSkills(n);
      if (n.popover === "model" || n.popover === "effort") checkSetting(n);
    });
  }
});
