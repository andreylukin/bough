import { describe, expect, test } from "bun:test";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { renderToStaticMarkup } from "react-dom/server";
import type { Row } from "../src/types";
import type { Scope } from "../src/api";
const { ChangesBodyView, FileCardView } = await import("../src/changes");
const { ChangesChipView } = await import("../src/app");

// go/tests/model/specs/ui_changes.fizz at the component level: every
// reachable settled state of the graph is rendered from props built from
// that node, and the node's meaning is asserted on the markup. The spec's
// own helpers (files, listed, chip, popbody, path, tabok, retryable) are
// mirrored below so each assertion reads as the spec's claim.
//
// Not in the markup, so not asserted here: pop (a <details> the browser
// opens; the popover's body is mounted open or shut, as the spec says),
// focus, back and palette (the DOM's focus and the palette's own state).
// The browser walk (tests/web/specs/model/ui_changes.spec.ts) owns those.

type Node = {
  vp: "wide" | "phone"; route: "thread" | "page"; pop: boolean; palette: boolean; focus: string; back: string;
  scope: Scope; sess: string; tree: string; pick: string; diff: "none" | "loading" | "ok"; edited: boolean;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_changes", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Changes#0.")) n[k.slice(10)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

// --- the spec's helpers
const files = (p: Node, s: Scope): string[] | null => {
  const st = s === "tree" ? p.tree : p.sess;
  if (st === "loading" || st === "failed") return null;
  if (s === "session") return p.edited ? ["a", "b"] : [];
  return p.edited ? ["a", "b", "h"] : ["h"];
};
const listed = (p: Node) => files(p, p.scope) ?? [];
const chip = (p: Node) => p.route === "thread" && p.sess !== "failed";
const popbody = (p: Node) => p.route === "thread" && p.vp === "wide" && chip(p);
const mounted = (p: Node) => p.route === "page" || popbody(p);
const noedits = (p: Node) => files(p, "session")?.length === 0;
const path = (p: Node) => {
  if (!mounted(p)) return "";
  if (p.pick) return p.pick;
  if (p.route === "thread" && listed(p).length === 1) return listed(p)[0];
  return "";
};
const retryState = (p: Node) => (p.scope === "tree" ? p.tree : p.sess);

// --- the world as the server answers it at a node
const row = { id: "s1", title: "t", cwd: "/repo", branch: "main", status: "done", live: false, archived: false, entries: 3 } as unknown as Row;
const CHANGE: Record<string, { path: string; add: number; del: number }> = {
  a: { path: "a", add: 3, del: 1 }, b: { path: "b", add: 2, del: 0 }, h: { path: "h", add: 1, del: 1 },
};
const PATCH = "@@ -1 +1 @@\n-old line\n+new line\n";
const AT = Date.UTC(2026, 8, 24, 12);

function read(p: Node, s: Scope) {
  const st = s === "tree" ? p.tree : p.sess;
  const fs = files(p, s);
  // A stale read keeps its list (files) and says the last refresh failed.
  return { files: fs && fs.map((f) => CHANGE[f]), repo: true, failed: st === "failed" || st === "stale", at: fs ? AT : undefined };
}
const data = (p: Node) => ({ session: read(p, "session"), tree: read(p, "tree"), turn: undefined, turnSeq: undefined, retry: () => {} });
/** The popover's fetched patch: none, Pending, or the text. */
const popDiff = (p: Node) => (p.diff === "ok" ? { path: path(p), text: PATCH } : p.diff === "loading" ? { path: path(p), text: null } : null);

const count = (html: string, re: RegExp) => (html.match(new RegExp(re.source, "g")) ?? []).length;
const noop = () => {};
const retryButtons = (html: string) => count(html, /<button class="btn rt-stop">Retry<\/button>/);
const tabs = (html: string) => [...html.matchAll(/<button[^>]*role="tab"[^>]*>/g)].map((m) => m[0]);

/** The tabs and the read's words, shared by the popover and the page. */
function checkReads(p: Node, html: string) {
  const t = tabs(html);
  expect(t.length).toBe(2);
  expect(t.map((x) => x.includes('aria-selected="true"'))).toEqual([p.scope === "session", p.scope === "tree"]);
  // Retry is on screen exactly while the shown scope's read failed or is stale (retryable, for a mounted body).
  const st = retryState(p);
  expect(retryButtons(html)).toBe(st === "failed" || st === "stale" ? 1 : 0);
  expect(html.includes("Couldn’t read the changes")).toBe(st === "failed");
  expect(html.includes("Stale: the last refresh failed")).toBe(st === "stale");
  const fs = files(p, p.scope);
  if (fs !== null && !fs.length) expect(html).toContain("This session has not changed any files");
}

function checkChip(p: Node) {
  const html = renderToStaticMarkup(
    <ChangesChipView row={row} data={data(p)} phone={p.vp === "phone"}><div className="probe" /></ChangesChipView>,
  );
  if (!chip(p)) {
    // A failed read is no value: no chip at all, so nothing to focus either.
    expect(html).toBe("");
    return;
  }
  const summary = /<summary aria-label="([^"]*)"/.exec(html);
  const link = /<a class="rt rt-link" href="#\/s\/s1\/changes" aria-label="([^"]*)"/.exec(html);
  const span = /^<span class="rt" aria-label="([^"]*)"/.exec(html);
  if (p.vp === "wide") {
    // PopoverOnlyOnWideThread: the popover's body lives under a <details> chip.
    expect(html).toMatch(/^<details class="rt rt-jobs">/);
    expect(html).toContain('<div class="probe"></div>');
    expect(html).toContain('<a class="link chg-full" href="#/s/s1/changes">Open full view</a>');
    expect(link).toBeNull();
  } else {
    // A phone has no popover: OpenPage from the chip is a link, unless there are no edits.
    expect(html).not.toContain("<details");
    expect(html).not.toContain("probe");
    expect(link === null).toBe(noedits(p));
    expect(span === null).toBe(!noedits(p));
  }
  const aria = (summary ?? link ?? span)![1];
  const edits = files(p, "session");
  if (edits === null) expect(aria).toMatch(/^Session edits: Reading…\./);
  else if (!edits.length) expect(aria).toMatch(/^No edits\./);
  else expect(aria).toMatch(/^Session edits: 2 files, 5 added, 1 removed\./);
  expect(aria.endsWith(", stale")).toBe(p.tree === "stale");
}

function checkPopover(p: Node) {
  const html = renderToStaticMarkup(
    <ChangesBodyView row={row} data={data(p)} scope={p.scope} onScope={noop} pick={p.pick || null} onPick={noop} diff={popDiff(p)} />,
  );
  checkReads(p, html);
  const fs = listed(p);
  // The popover lists files unless its one file is shown at once (haslist).
  const rows = [...html.matchAll(/<button class="rt-link rt-file[^"]*"[^>]*title="([^"]*)"/g)].map((m) => m[1]);
  expect(rows).toEqual(fs.length > 1 ? fs : []);
  // SelectionIsListed: the picked row is one the list has, and it is the one marked.
  const current = [...html.matchAll(/<button class="rt-link rt-file chg-on"[^>]*title="([^"]*)"[^>]*aria-current="true"/g)].map((m) => m[1]);
  if (p.pick) expect(fs).toContain(p.pick);
  expect(current).toEqual(fs.length > 1 && path(p) ? [path(p)] : []);
  // DiffOnlyForShownFile: a diff pane exactly while a file is shown, for that file.
  const shown = path(p);
  expect(count(html, /class="chg-diff"/)).toBe(shown ? 1 : 0);
  expect(p.diff === "none").toBe(shown === "");
  if (shown) {
    expect(html).toContain(`<div class="chg-diff-head"><span class="mono rt-job-cmd" title="${shown}">${shown}</span>`);
    expect(html.includes('aria-busy="true"')).toBe(p.diff === "loading");
    expect(html.includes("Loading diff…")).toBe(p.diff === "loading");
    expect(html.includes("rt-diff-body")).toBe(p.diff === "ok");
  } else {
    expect(html).not.toContain("rt-diff-body");
    expect(html).not.toContain("Loading diff…");
  }
}

function checkPage(p: Node) {
  const html = renderToStaticMarkup(
    <ChangesBodyView row={row} data={data(p)} scope={p.scope} onScope={noop} cards pick={null} onPick={noop} diff={null} />,
  );
  checkReads(p, html);
  // PageOpensOnSessionEdits.
  if (p.sess === "loading") expect(p.scope).toBe("session");
  // A tab with an empty list is disabled unless it is the shown one (tabok).
  for (const [i, s] of (["session", "tree"] as Scope[]).entries()) {
    const fs = files(p, s);
    expect(tabs(html)[i].includes('disabled=""')).toBe(fs !== null && fs.length === 0 && p.scope !== s);
  }
  const fs = listed(p);
  const heads = [...html.matchAll(/<summary class="chg-card-head" title="([^"]*)"/g)].map((m) => m[1]);
  expect(heads).toEqual(fs);
  // SelectionIsListed, and DiffOnlyForShownFile per card: the one open card
  // is the picked file, it alone fetches, and a closed card shows no diff.
  if (p.pick) expect(fs).toContain(p.pick);
  expect(p.diff === "none").toBe(p.pick === "");
  for (const f of fs) {
    const open = f === p.pick;
    const card = renderToStaticMarkup(
      <FileCardView row={row} file={CHANGE[f]} scope={p.scope} open={open} onToggle={noop} onRetry={noop}
                    diff={open && p.diff === "ok" ? { text: PATCH } : { text: null }} />,
    );
    expect(/<details class="chg-card"[^>]*>/.exec(card)![0].includes('open=""')).toBe(open);
    expect(card.includes("Loading diff…")).toBe(open && p.diff === "loading");
    expect(card.includes("rt-diff-body")).toBe(open && p.diff === "ok");
  }
}

describe("ui_changes.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(100);
  });

  for (const [key, n] of nodes) {
    test(key, () => {
      // PaletteOwnsFocus and FocusOnMountedChip are the spec's own; the chip's
      // absence below (a failed read, a phone) is what leaves no summary to hold focus.
      expect(n.palette).toBe(n.focus === "palette");
      if (n.focus === "chip" || n.back === "chip") expect(popbody(n)).toBe(true);
      if (n.pop) expect(popbody(n) && !n.palette).toBe(true);
      if (n.route === "thread") {
        checkChip(n);
        if (popbody(n)) checkPopover(n);
        // Nothing of the popover is mounted without its chip: no pick, no diff.
        else expect([n.pick, n.diff]).toEqual(["", "none"]);
      } else {
        checkPage(n);
      }
    });
  }
});
