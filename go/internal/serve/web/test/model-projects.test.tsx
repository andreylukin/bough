import { expect, test } from "bun:test";
import { readFileSync } from "fs";
import { join } from "path";
import { renderToStaticMarkup } from "react-dom/server";
import { DialogView, textBlocked, type DialogReq } from "../src/dialog";
import { ErrorToast } from "../src/loading";
import { ProjectsView } from "../src/projects";
import type { Project } from "../src/types";

// Every reachable state of go/tests/model/specs/ui_projects.fizz,
// rendered. The Playwright walk (tests/web/specs/model/ui_projects.spec.ts)
// drives the same graph through a real serve at ~half a second a node;
// this renders each node's surface — the Projects page, the one dialog,
// the delete toast — from the node's fields and checks what the node
// promises about controls, dialogs and aria, in milliseconds. Focus and
// opener are DOM state static markup cannot show: the walk owns those.
//
// The nodes come from the generated graph itself (nodes_*.pb, protobuf
// `Nodes { repeated string json = 1; }`, see tests/model/GRAPH.md), so a
// spec change that adds a state adds it here without an edit.

interface Node {
  disk: string; shown: string; menu: boolean; dialog: string; draft: string;
  saving: boolean; failed: boolean; toast: boolean; focus: string; opener: string;
}

function nodes(): Node[] {
  const b = readFileSync(join(import.meta.dir, "../../../../tests/model/testdata/ui_projects/nodes_000000_of_000000.pb"));
  let i = 0;
  const varint = () => { let r = 0, s = 0; for (;;) { const c = b[i++]; r += (c & 0x7f) * 2 ** s; s += 7; if (!(c & 0x80)) return r; } };
  const out: Node[] = [];
  while (i < b.length) {
    const tag = varint(), len = varint();
    if (tag === 0x0a) out.push(JSON.parse(b.toString("utf8", i, i + len)).roles[0].fields);
    i += len;
  }
  return out;
}

const SLUG = "alpha";
const noop = () => {};
const count = (html: string, re: RegExp) => (html.match(new RegExp(re, "g")) ?? []).length;

// The dialog the node has up, as projects.tsx asks it, with the box's
// text classified the way the spec's draft is.
function dialogOf(n: Node): { req: DialogReq; text: string } | null {
  const base = { kind: "text" as const, allowEmpty: false, resolve: noop, onSubmit: async () => {} };
  // The rename opened on the name the list showed; a list that has since
  // emptied no longer says which, and nothing here depends on it.
  const name = n.shown === "none" ? "Alpha" : n.shown;
  switch (n.dialog) {
    case "none": return null;
    case "create":
      return { req: { ...base, title: "New project", initial: "", placeholder: "What is this work?", action: "Create" },
               text: { blocked: "", good: "Alpha", bad: "!!!" }[n.draft]! };
    case "rename":
      // Blocked is the box still reading the current name, as it opens.
      return { req: { ...base, title: "Rename project", initial: name, action: "Rename" },
               text: { blocked: name, good: name === "Alpha" ? "Beta" : "Alpha" }[n.draft]! };
    case "delete":
      return { req: { ...base, title: `Delete “${name}”?`, body: `Type ${SLUG} to confirm.`, initial: "", placeholder: SLUG,
                      action: "Delete project", danger: true },
               text: { blocked: "", good: SLUG, bad: "alpah" }[n.draft]! };
  }
  throw new Error(`ui_projects: unknown dialog ${n.dialog}`);
}

function surface(n: Node): string {
  const projects = n.shown === "none" ? [] : [{ slug: SLUG, name: n.shown } as Project];
  const d = dialogOf(n);
  return renderToStaticMarkup(
    <>
      <ProjectsView projects={projects} rows={[]} onOpen={noop} onAssign={noop} onAssignMany={async () => []}
                    onCreate={async () => ({ slug: SLUG })} onRename={async () => {}} onDelete={noop}
                    openMenu={n.menu ? SLUG : undefined} />
      {d && <DialogView req={d.req} text={d.text} saving={n.saving} failed={n.failed ? "project not found" : ""}
                        onText={noop} onSubmit={noop} onDismiss={noop} />}
      {n.toast && <ErrorToast toast={{ label: "delete the project", msg: "project not found", retry: noop }} busy={false} onDismiss={noop} />}
    </>,
  );
}

const all = nodes();

test("the graph is the spec's: 128 states, each distinct", () => {
  expect(all.length).toBe(128);
  expect(new Set(all.map((n) => JSON.stringify(n))).size).toBe(all.length);
});

test("the box's text is classified as the spec's draft by the dialog's own rule", () => {
  for (const n of all) {
    const d = dialogOf(n);
    if (d) expect({ n, blocked: textBlocked(d.req, d.text) }).toEqual({ n, blocked: n.draft === "blocked" });
  }
});

test("every ui_projects node renders what it promises", () => {
  for (const n of all) {
    const html = surface(n);
    const at = (what: string, ok: boolean) => { if (!ok) throw new Error(`${what} at ${JSON.stringify(n)}\n${html}`); };

    // The status: the page head is up in every state.
    at("status", html.includes("<h1 title=") && html.includes(">Projects</h1>"));

    // Exactly one New project… button, the empty state's or the head's.
    at("one New project…", count(html, /<button[^>]*>New project…<\/button>/) === 1);
    const empty = html.includes("proj-empty-row");
    at("empty state iff nothing listed", empty === (n.shown === "none"));
    at("No projects yet. iff nothing listed", html.includes("No projects yet.") === (n.shown === "none"));

    // The listed section: its name, and one ⋯ whose aria-expanded is the menu.
    const more = html.match(/<button class="btn btn-ghost btn-sm proj-more"[^>]*>/g) ?? [];
    if (n.shown === "none") {
      at("no ⋯ with nothing listed", more.length === 0);
      at("no section", !html.includes(`href="#/projects/${SLUG}"`));
    } else {
      at("section name", html.includes(`<h2>${n.shown}</h2>`));
      at("one ⋯", more.length === 1);
      at("⋯ names the project", more[0].includes(`aria-label="More actions for ${n.shown}"`));
      at("⋯ aria-expanded is the menu", more[0].includes(`aria-expanded="${n.menu}"`));
      at("⋯ has a menu popup", more[0].includes(`aria-haspopup="menu"`));
    }

    // The menu: Rename… and Delete…, only while open.
    at("menu count", count(html, /role="menu"/) === (n.menu ? 1 : 0));
    at("menuitems", count(html, /role="menuitem"/) === (n.menu ? 2 : 0));
    if (n.menu) at("menu items", /role="menuitem"[^>]*>Rename…<\/button><button role="menuitem" class="proj-menu-danger"[^>]*>Delete…</.test(html));

    // The one dialog, of the node's kind.
    const dialogs = count(html, /role="dialog"/);
    at("dialog count", dialogs === (n.dialog === "none" ? 0 : 1));
    at("menu or dialog, never both", !(dialogs && count(html, /role="menu"/)));
    // The toast: exactly while the node has it.
    at("toast", count(html, /<div class="toast"/) === (n.toast ? 1 : 0));
    if (n.toast) at("toast title", html.includes("Couldn’t delete the project") && html.includes('aria-label="Dismiss"'));
    if (n.dialog === "none") {
      at("no dialog input", !html.includes("dlg-input"));
      at("no failure outside a dialog", !html.includes("Not saved:"));
      continue;
    }

    const dlg = html.slice(html.indexOf('<div class="dlg-scrim"'));
    const title = /<h2 id="dlg-title" class="dlg-title">([^<]*)<\/h2>/.exec(dlg)?.[1] ?? "";
    const kind = title === "New project" ? "create" : title === "Rename project" ? "rename" : title.startsWith("Delete") ? "delete" : title;
    at("dialog kind", kind === n.dialog);
    at("aria-modal", /role="dialog" aria-modal="true" aria-labelledby="dlg-title"/.test(dlg));
    at("aria-busy iff saving", dlg.includes('aria-busy="true"') === n.saving);

    const input = /<input[^>]*class="field dlg-input"[^>]*\/?>/.exec(dlg)?.[0] ?? "";
    at("one input, labelled", input.includes('aria-labelledby="dlg-title"'));
    at("read-only iff saving", /readOnly=""|readonly=""/.test(input) === n.saving);
    at("aria-invalid iff failed", input.includes('aria-invalid="true"') === n.failed);
    at("aria-describedby iff failed", input.includes('aria-describedby="dlg-err"') === n.failed);
    at("alert iff failed", /<p id="dlg-err" class="dlg-err" role="alert">Not saved: /.test(dlg) === n.failed);

    const buttons = dlg.slice(dlg.indexOf('class="dlg-actions"')).match(/<button[^>]*>[^<]*<\/button>/g) ?? [];
    at("Cancel and a primary", buttons.length === 2);
    const [cancel, primary] = buttons;
    at("Cancel disabled iff saving", cancel.includes('disabled=""') === n.saving && cancel.includes(">Cancel<"));
    // Busy is aria-disabled, not disabled: disabling the focused button
    // dropped focus to <body> (ui_dialogs walk).
    at("primary disabled iff blocked", / disabled=""/.test(primary) === (n.draft === "blocked"));
    at("primary aria-disabled iff saving", primary.includes('aria-disabled="true"') === n.saving);
    at("delete's primary is the danger one", primary.includes("btn-danger") === (n.dialog === "delete"));
    const label = n.saving ? { create: "Creating…", rename: "Renaming…" }[n.dialog]
      : { create: "Create", rename: "Rename", delete: "Delete project" }[n.dialog];
    at(`primary reads ${label}`, primary.endsWith(`>${label}</button>`));
  }
});
