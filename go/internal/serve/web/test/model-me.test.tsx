import { describe, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { loadGraph } from "../../../../tests/web/model/graph.ts";
import { MePageView, SteerView, type SteerState } from "../src/me";
import type { MeData, MeSignal, WikiBlock } from "../src/wiki";

// go/tests/model/specs/ui_me.fizz at the component level: every settled
// state of the checked-in graph is rendered from props built from that
// node, the page half of the node is read back off the markup the way the
// browser walk's readUiState reads the DOM, and the spec's invariants are
// asserted on what shows. The browser walk (tests/web/specs/model/ui_me.spec.ts)
// drives the same graph through a real serve; this names a broken render
// in milliseconds.

type Node = {
  srv_profile: boolean; srv_brief: string; srv_signal: string; run: string;
  data: string; profile: boolean; brief: string; signal: string; refreshing: boolean; menu: boolean; steer: string;
};

const graph = loadGraph(new URL("../../../../tests/model/testdata/ui_me", import.meta.url).pathname);
const nodes = new Map<string, Node>();
for (const { name, state } of graph.nodes) {
  if (name !== "yield") continue;
  const n: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(state)) if (k.startsWith("Me#0.")) n[k.slice(5)] = v;
  nodes.set(JSON.stringify(n, Object.keys(n).sort()), n as Node);
}

const TITLE = "Review acme/app#1";
const KEY = "gh:acme/app#1";
const READ_FAIL = "Read failed";
const STEER_FAIL = "Profile write failed";
const SAID = "Filed under Watch. The next brief reads it.";
const signal: MeSignal = { kind: "needs-you", source: "gh", title: TITLE, cite: KEY, repo: "acme/app", author: "octo" };
const blocks: WikiBlock[] = [
  { kind: "heading", text: "Today", line: 1, end: 1, raw: "## Today", bullet: false, cites: [] },
  { kind: "claim", text: "Ship the Me walk.", line: 3, end: 3, raw: "- Ship the Me walk.", bullet: true, state: "cited", cites: [] },
];

/** What the last good read answered at a node: the page half, never the server's. */
function meData(n: Node): MeData {
  const path = "topics/me/briefs/2026-09-24.md";
  return {
    date: "2026-09-24", hasProfile: n.profile, days: ["2026-09-24"],
    ...(n.brief === "today" ? {
      path, asOf: new Date(Date.UTC(2026, 8, 24, 12)).toISOString(),
      page: { path, topic: "me", title: "Brief", summary: "", updated: "", body: "", sessions: [], linkedFrom: [], blocks,
        counts: { cited: 1, inferred: 0, uncited: 0, unsupported: 0, superseded: 0 } },
    } : {}),
    signals: n.signal === "none" ? null : { items: [signal], sources: [{ name: "gh", ok: true }] },
    triage: {
      pinned: n.signal === "pinned" ? [KEY] : [],
      dismissed: n.signal === "dismissed" ? { [KEY]: "" } : {},
    },
  };
}

/** What Steer holds at a node. */
function steerState(n: Node): SteerState {
  if (n.steer === "said") return { text: "", busy: false, said: SAID, err: "" };
  if (n.steer === "error") return { text: "watch acme/app", busy: false, said: "", err: STEER_FAIL };
  return { text: "", busy: false, said: "", err: "" };
}

const noop = () => {};
function render(n: Node): string {
  return renderToStaticMarkup(
    <MePageView data={n.data === "loaded" ? meData(n) : null} error={n.data === "error" ? READ_FAIL : ""}
                refreshing={n.refreshing} menuFor={n.menu ? KEY : null} onMenu={noop}
                steer={<SteerView state={steerState(n)} onText={noop} onSend={noop} />}
                onRefresh={noop} onRetry={noop} onTriage={noop} onOpenPage={noop} onOpenSession={noop} />,
  );
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
const button = (html: string, text: string) => new RegExp(`<button[^>]*>${text}</button>`).exec(html)?.[0] ?? "";

/** readUiState from ui_me.spec.ts, on markup: the page half of the node as the screen says it. */
function readMarkup(html: string) {
  const loaded = html.includes('<div class="me">');
  const steer = element(html, /<form class="me-steer"[^>]*>/);
  const pinned = element(html, /<section class="me-group" data-kind="pinned"[^>]*>/);
  const bar = element(html, /<header class="me-bar">/);
  return {
    data: loaded ? "loaded" : /class="thread me-loading"[\s\S]*<h2[^>]*>Couldn’t read the brief<\/h2>/.test(html) ? "error" : "loading",
    profile: loaded && !html.includes(">Tell the brief whose work this is</h2>"),
    brief: html.includes('<article class="me-brief"') ? "today" : "none",
    signal: html.includes('class="me-sig-wrap') ? (pinned.includes('class="me-sig-wrap') ? "pinned" : "shown")
      : html.includes("me-dismissed") ? "dismissed" : "none",
    refreshing: button(bar, "Writing…") !== "",
    menu: /role="menu" aria-label="Dismiss"/.test(html),
    steer: /role="status"/.test(steer) ? "said" : /role="alert"/.test(steer) ? "error" : "",
  };
}

function check(n: Node) {
  const html = render(n);
  const { srv_profile, srv_brief, srv_signal, run, ...page } = n;
  // The screen says exactly the page half of the node.
  expect(readMarkup(html)).toEqual(page);

  if (n.data !== "loaded") {
    expect(html).not.toContain('<div class="me">');
    if (n.data === "loading") {
      expect(html).toMatch(/role="status"[^>]*aria-busy="true"/);
      expect(button(html, "Retry")).toBe("");
    } else {
      expect(html).toContain(READ_FAIL);
      expect(button(html, "Retry")).not.toBe("");
      expect(button(html, "Retry")).not.toContain("disabled");
      expect(html).not.toContain('aria-busy="true"');
    }
    return;
  }

  // OneScreen: exactly one of the three bodies.
  const screens = [
    html.includes(">Tell the brief whose work this is</h2>"),
    html.includes(">No brief yet today</h2>"),
    html.includes('<article class="me-brief"'),
  ];
  expect(screens.filter(Boolean).length).toBe(1);
  if (!n.profile) expect(button(html, "Write your profile")).not.toBe("");
  if (n.profile && n.brief === "none") expect(button(html, "Write it now")).not.toBe("");

  // RefreshNeedsProfile: the bar's button exists exactly with a profile,
  // and says Writing… (disabled) exactly while refreshing.
  const bar = element(html, /<header class="me-bar">/);
  const refresh = /<button[^>]*class="btn btn-sm btn-primary"[^>]*>([^<]*)<\/button>/.exec(bar);
  expect(refresh !== null).toBe(n.profile);
  if (refresh) {
    expect(refresh[1]).toBe(n.refreshing ? "Writing…" : "Refresh");
    expect(refresh[0].includes("disabled")).toBe(n.refreshing);
  }
  // A brief on screen can be copied as a standup.
  expect(button(bar, "Copy standup") !== "").toBe(n.brief === "today");

  // SteerNeedsBrief: the steering line is under a brief with a profile, and its note is the node's.
  const steer = element(html, /<form class="me-steer"[^>]*>/);
  expect(steer !== "").toBe(n.profile && n.brief === "today");
  if (steer) {
    expect(steer.includes(SAID)).toBe(n.steer === "said");
    expect(steer.includes(STEER_FAIL)).toBe(n.steer === "error");
    expect(count(steer, /role="(status|alert)"/)).toBe(n.steer === "" ? 0 : 1);
    // The failed sentence stays in the box for another try.
    expect(/<input[^>]*value="([^"]*)"/.exec(steer)![1]).toBe(n.steer === "error" ? "watch acme/app" : "");
  }

  // The row, its pin and its Dismiss menu.
  const rows = count(html, /class="me-sig-wrap/);
  expect(rows).toBe(n.signal === "shown" || n.signal === "pinned" ? 1 : 0);
  expect(html.includes("1 dismissed row hidden.")).toBe(n.signal === "dismissed");
  if (rows) {
    const pin = /<button[^>]*aria-label="(?:Un)?[pP]in [^"]*"[^>]*>/.exec(html)![0];
    expect(pin).toContain(`aria-label="${n.signal === "pinned" ? "Unpin" : "Pin"} ${TITLE}"`);
    expect(pin).toContain(`aria-pressed="${n.signal === "pinned"}"`);
    const x = /<button[^>]*aria-label="Dismiss [^"]*"[^>]*>/.exec(html)![0];
    expect(x).toContain(`aria-expanded="${n.menu}"`);
    const groups = [...html.matchAll(/<section class="me-group" data-kind="([^"]+)"/g)].map((m) => m[1]);
    expect(groups).toEqual([n.signal === "pinned" ? "pinned" : "needs-you"]);
  }
  // MenuNeedsRow: an open menu is the row's, with its three ways to dismiss.
  const menu = element(html, /<div class="me-menu" role="menu"[^>]*>/);
  expect(menu !== "").toBe(n.menu);
  if (menu) {
    expect(rows).toBe(1);
    expect([...menu.matchAll(/role="menuitem"[^>]*>([^<]*)</g)].map((m) => m[1]))
      .toEqual(["Just this one", "Nothing from acme/app", "Nothing from octo"]);
  }
}

describe("ui_me.fizz, every node rendered", () => {
  test("the graph has states to render", () => {
    expect(nodes.size).toBeGreaterThan(1);
  });
  for (const [key, n] of nodes) test(key, () => check(n));
});
