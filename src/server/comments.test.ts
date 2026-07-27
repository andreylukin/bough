/**
 * Tests for artifact comments.
 *
 * The acceptance criteria (plan T6.7) are two:
 *
 *   - **The sidecar never appears in `listArtifacts`.** Asserted here at the layout
 *     level — the file is outside the artifact tree by construction — and end to end
 *     in `artifacts.test.ts`, which also proves it is unreachable through the artifact
 *     route. Two tests because the failure has two independent shapes, and either one
 *     alone would pass while the other leaked (plan §6.12).
 *   - **Send posts a system note.** Not "writes a message row": the note has to go
 *     through `postSystemNote`, because that is where the wake rule lives — one turn
 *     per batch, riding the queued drain on a busy session and never a second
 *     concurrent turn (`agents/notes.ts`).
 *
 * The rest covers what the widget's own fetches do: add, list by artifact, delete, and
 * the two things a page can do wrong — a corrupt sidecar and a nonsense anchor — both
 * of which must degrade rather than break the page.
 *
 * Assertions come from `node:assert/strict`: jsr.io is not reachable here.
 */
import assert from "node:assert/strict";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { PathError } from "../errors.ts";
import { listArtifacts, publishArtifact } from "../hostfn/artifact.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx } from "../types.ts";
// `./app.ts` first — see the note in `artifacts.test.ts` about the documented cycle.
import { createHandler, type Route, route } from "./app.ts";
import {
  addComment,
  type ArtifactComment,
  COMMENTS_NOTE_PREFIX,
  commentsPath,
  commentWidget,
  deleteComment,
  deleteCommentH,
  formatForAgent,
  listCommentsH,
  loadComments,
  markSent,
  postCommentH,
  sendCommentsH,
} from "./comments.ts";

// ---- fixtures ---------------------------------------------------------------

const TABLE: Route[] = [
  route("GET", "/sessions/:id/comments", listCommentsH),
  route("POST", "/sessions/:id/comments", postCommentH),
  route("POST", "/sessions/:id/comments/send", sendCommentsH),
  route("DELETE", "/sessions/:id/comments/:cid", deleteCommentH),
];

function tmp(): string {
  return Deno.makeTempDirSync({ prefix: "bough-comments-" });
}

const anchor = { label: "Files touched", selector: "body > h2", xf: 0.5, yf: 0.3 };

async function withBoughHome(body: (home: string) => Promise<void> | void): Promise<void> {
  const home = tmp();
  const previous = Deno.env.get("BOUGH_HOME");
  Deno.env.set("BOUGH_HOME", home);
  try {
    await body(home);
  } finally {
    if (previous === undefined) Deno.env.delete("BOUGH_HOME");
    else Deno.env.set("BOUGH_HOME", previous);
    Deno.removeSync(home, { recursive: true });
  }
}

function session(id: string): Session {
  return { id, title: id, kind: "root", parentId: null, createdAt: Date.now() };
}

function fixture() {
  const db = openDb(":memory:");
  const bus = new Bus({ onListenerError: () => {} });
  const ctx: AppCtx = { db, bus, model: "test-model" };
  return { call: createHandler(ctx, { routes: TABLE }), db, bus, ctx };
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;
const get = (path: string) => new Request(url(path));
const post = (path: string, body: unknown) =>
  new Request(url(path), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
const del = (path: string) => new Request(url(path), { method: "DELETE" });

// ---- storage ----------------------------------------------------------------

Deno.test("addComment persists, loadComments reads back, deleteComment removes", () => {
  const dir = tmp();
  try {
    const c = addComment("s1", { artifact: "index.html", text: "this list is stale", anchor }, {
      dir,
    });
    assert.equal(typeof c.id, "string");
    assert.equal(c.sent, false);
    assert.equal(c.anchor.label, "Files touched");
    assert.equal(loadComments("s1", { dir }).length, 1);
    assert.equal(loadComments("s1", { dir })[0].text, "this list is stale");
    assert.equal(deleteComment("s1", c.id, { dir }), true);
    assert.deepEqual(loadComments("s1", { dir }), []);
    assert.equal(deleteComment("s1", "nope", { dir }), false);
  } finally {
    Deno.removeSync(dir, { recursive: true });
  }
});

Deno.test("markSent flips only the named notes", () => {
  const dir = tmp();
  try {
    const a = addComment("s2", { artifact: "index.html", text: "one", anchor }, { dir });
    const b = addComment("s2", { artifact: "index.html", text: "two", anchor }, { dir });
    markSent("s2", [a.id], { dir });
    const all = loadComments("s2", { dir });
    assert.equal(all.find((c) => c.id === a.id)!.sent, true);
    assert.equal(all.find((c) => c.id === b.id)!.sent, false);
  } finally {
    Deno.removeSync(dir, { recursive: true });
  }
});

Deno.test("a corrupt sidecar reads as empty rather than breaking the page", () => {
  const dir = tmp();
  try {
    Deno.writeTextFileSync(join(dir, "s3.json"), "{not json at all");
    assert.deepEqual(loadComments("s3", { dir }), []);
    // …and a new note still saves over it, so the page stays usable.
    const c = addComment("s3", { artifact: "x.html", text: "still works", anchor }, { dir });
    assert.deepEqual(loadComments("s3", { dir }).map((x) => x.id), [c.id]);
  } finally {
    Deno.removeSync(dir, { recursive: true });
  }
});

Deno.test("an unusable anchor stores a centered default — the text is the point", () => {
  const dir = tmp();
  try {
    const c = addComment("s4", { artifact: "x.html", text: "note", anchor: "nonsense" }, { dir });
    assert.deepEqual(c.anchor, { label: "", selector: "", xf: 0.5, yf: 0.5 });
    const d = addComment("s4", { artifact: "x.html", text: "note" }, { dir });
    assert.equal(d.anchor.xf, 0.5);
  } finally {
    Deno.removeSync(dir, { recursive: true });
  }
});

Deno.test("a traversing session id cannot steer the sidecar write", () => {
  const dir = tmp();
  const outside = tmp();
  try {
    for (const bad of ["../evil", "../../evil", "a/b", "sub/../../evil", "", outside]) {
      assert.throws(() => commentsPath(bad, { dir }), PathError, `id ${JSON.stringify(bad)}`);
      assert.throws(() => addComment(bad, { artifact: "x", text: "t", anchor }, { dir }));
      assert.deepEqual(loadComments(bad, { dir }), []); // reads are safe-empty
    }
    assert.deepEqual([...Deno.readDirSync(dir)].map((e) => e.name), []);
    assert.deepEqual([...Deno.readDirSync(outside)].map((e) => e.name), []);

    // `..` is not an escape here, because the sidecar name is `<id>.json`: it lands on
    // `...json` INSIDE the store. Asserted rather than assumed — the interesting
    // property is that nothing leaves `dir`, not that every odd id is refused.
    const odd = addComment("..", { artifact: "x", text: "t", anchor }, { dir });
    assert.equal(commentsPath("..", { dir }), join(dir, "...json"));
    assert.deepEqual(loadComments("..", { dir }).map((c) => c.id), [odd.id]);
    assert.deepEqual([...Deno.readDirSync(outside)].map((e) => e.name), []);
  } finally {
    Deno.removeSync(dir, { recursive: true });
    Deno.removeSync(outside, { recursive: true });
  }
});

// ---- AC: the sidecar is not walked by listArtifacts -------------------------

Deno.test("AC: the sidecar is outside the artifact tree and never listed", async () => {
  await withBoughHome(async (home) => {
    await publishArtifact("s5", "index.html", "<h1>hi</h1>");
    addComment("s5", { artifact: "index.html", text: "note", anchor });

    const sidecar = commentsPath("s5");
    assert.equal(Deno.statSync(sidecar).isFile, true);
    // A SIBLING of the artifacts tree, never inside it — the whole invariant.
    assert.equal(sidecar.startsWith(join(home, "artifacts")), false);
    assert.equal(sidecar, join(home, "comments", "s5.json"));

    assert.deepEqual(listArtifacts("s5").map((a) => a.name), ["index.html"]);
  });
});

// ---- the agent-facing note --------------------------------------------------

Deno.test("formatForAgent groups by artifact and names the anchor", () => {
  const comments: ArtifactComment[] = [
    { id: "1", artifact: "index.html", text: "fix this", anchor, ts: 1, sent: false },
    {
      id: "2",
      artifact: "chart.html",
      text: "wrong axis",
      anchor: { ...anchor, label: "" },
      ts: 2,
      sent: false,
    },
  ];
  const note = formatForAgent(comments);
  assert.equal(note.startsWith(COMMENTS_NOTE_PREFIX), true);
  assert.equal(note.includes("left 2 comments"), true);
  assert.equal(note.includes('On the artifact "index.html"'), true);
  assert.equal(note.includes('On the artifact "chart.html"'), true);
  assert.equal(note.includes('1. (near "Files touched") fix this'), true);
  assert.equal(note.includes("1. wrong axis"), true); // no anchor → no "(near …)"
  assert.equal(note.includes("Address the comments, or reply with questions."), true);
});

Deno.test("formatForAgent stays singular for one comment on one artifact", () => {
  const note = formatForAgent([
    { id: "1", artifact: "a.html", text: "t", anchor, ts: 1, sent: false },
  ]);
  assert.equal(note.includes("left 1 comment on the artifact"), true);
});

// ---- routes -----------------------------------------------------------------

Deno.test("POST adds a note; GET filters by artifact; DELETE removes it", async () => {
  await withBoughHome(async () => {
    const { call, db } = fixture();
    db.createSession(session("sA"));

    const created = await call(post("/sessions/sA/comments", {
      artifact: "index.html",
      text: "stale",
      anchor,
    }));
    assert.equal(created.status, 201);
    const note = await created.json() as ArtifactComment;

    await call(post("/sessions/sA/comments", {
      artifact: "chart.html",
      text: "axis",
      anchor,
    })).then((r) => r.json());

    const all = await (await call(get("/sessions/sA/comments"))).json() as {
      comments: ArtifactComment[];
    };
    assert.equal(all.comments.length, 2);

    const filtered = await (await call(get("/sessions/sA/comments?artifact=chart.html")))
      .json() as {
        comments: ArtifactComment[];
      };
    assert.deepEqual(filtered.comments.map((c) => c.text), ["axis"]);

    const removed = await call(del(`/sessions/sA/comments/${note.id}`));
    assert.equal(removed.status, 200);
    await removed.json();
    const missing = await call(del(`/sessions/sA/comments/${note.id}`));
    assert.equal(missing.status, 404);
    await missing.json();
  });
});

Deno.test("posting a comment to an unknown session is a 404, not a stray file", async () => {
  await withBoughHome(async (home) => {
    const { call } = fixture();
    const res = await call(post("/sessions/ghost/comments", {
      artifact: "index.html",
      text: "t",
      anchor,
    }));
    assert.equal(res.status, 404);
    await res.json();
    assert.throws(() => Deno.statSync(join(home, "comments", "ghost.json")));
  });
});

Deno.test("AC: send posts ONE system note for the batch and marks them sent", async () => {
  await withBoughHome(async () => {
    const { call, db, bus } = fixture();
    db.createSession(session("sB"));
    addComment("sB", { artifact: "index.html", text: "first", anchor });
    addComment("sB", { artifact: "index.html", text: "second", anchor });
    addComment("sB", { artifact: "chart.html", text: "third", anchor });

    const started: Message[] = [];
    bus.subscribe((e) => {
      if (e.type === "message.started") started.push(e.data as Message);
    });

    const res = await call(post("/sessions/sB/comments/send", {}));
    assert.equal(res.status, 200);
    assert.equal(((await res.json()) as { sent: number }).sent, 3);

    // One message, not three — the agent should see the whole review at once.
    assert.equal(started.length, 1);
    const message = started[0];
    assert.equal(message.role, "system");
    assert.equal(message.pending, false);
    const text = message.parts.map((p) => (p.type === "text" ? p.text : "")).join("");
    assert.equal(text.startsWith(COMMENTS_NOTE_PREFIX), true);
    assert.equal(text.includes("first"), true);
    assert.equal(text.includes("third"), true);

    // …and it is persisted on the thread the agent replays, not just announced.
    assert.equal(db.messagesFor("sB").length, 1);

    // Every note is now marked sent, so a second click is a no-op.
    assert.equal(loadComments("sB").every((c) => c.sent), true);
    const again = await call(post("/sessions/sB/comments/send", {}));
    assert.equal(((await again.json()) as { sent: number }).sent, 0);
    assert.equal(started.length, 1);
  });
});

Deno.test("send can deliver a named subset and leaves the rest unsent", async () => {
  await withBoughHome(async () => {
    const { call, db } = fixture();
    db.createSession(session("sC"));
    const a = addComment("sC", { artifact: "index.html", text: "one", anchor });
    addComment("sC", { artifact: "index.html", text: "two", anchor });

    const res = await call(post("/sessions/sC/comments/send", { ids: [a.id] }));
    assert.equal(((await res.json()) as { sent: number }).sent, 1);
    const after = loadComments("sC");
    assert.equal(after.find((c) => c.id === a.id)!.sent, true);
    assert.equal(after.filter((c) => !c.sent).length, 1);
  });
});

Deno.test("sending into an unknown session is a 404 with nothing delivered", async () => {
  await withBoughHome(async () => {
    const { call } = fixture();
    const res = await call(post("/sessions/ghost/comments/send", {}));
    assert.equal(res.status, 404);
    await res.json();
  });
});

// ---- the injected widget ----------------------------------------------------

Deno.test("the widget is self-contained: no external network references", () => {
  const w = commentWidget();
  assert.equal(/src=["']https?:/i.test(w), false);
  assert.equal(/href=["']https?:/i.test(w), false);
  assert.equal(/cdn\.|googleapis|unpkg|jsdelivr|fonts\./i.test(w), false);
  // It talks to the same origin, by relative path only.
  assert.equal(w.includes('"/sessions/"'), true);
});

Deno.test("the widget interpolates nothing — it reads its identity from location", () => {
  // Called twice, byte-identical: there is no per-session or per-artifact templating,
  // so the layer cannot inject anything into the page it is spliced into.
  assert.equal(commentWidget(), commentWidget());
  assert.equal(commentWidget().includes("location.pathname"), true);
});
