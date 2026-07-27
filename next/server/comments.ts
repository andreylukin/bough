/**
 * Artifact comments — the user's margin notes on a published page, left for the
 * agent to read.
 *
 * WHY THIS EXISTS. An artifact is where the agent's work becomes something a human
 * can actually look at: a diff review, a comparison table, a chart, a clickable
 * prototype. Feedback on it is inherently positional — "this column is wrong", "this
 * row is stale" — and re-typing "the third card under Findings" into the composer
 * loses exactly the part that made the page worth publishing. So the page itself
 * carries the annotation UI: toggle comment mode, click anywhere to pin a note, send
 * the batch, and it arrives as one `[artifact comments]` system message the agent
 * acts on (spec §11).
 *
 * THE INVARIANT THIS HOLDS: **the sidecar lives OUTSIDE the artifact directory.**
 * Comments persist as one JSON file per session at `~/.bough/comments/<id>.json`
 * (`paths.ts`), a sibling of `~/.bough/artifacts/` — never inside
 * `~/.bough/artifacts/<id>/`. Put it inside and two things break at once, both
 * silently: `listArtifacts` walks it and shows the user a file they never published,
 * and `GET /artifacts/<id>/comments.json` serves the whole note history to anything
 * that asks for it, including the artifact's own scripts. Neither failure announces
 * itself, which is why this is a stated invariant rather than a layout preference
 * (plan §6.12).
 *
 * The filesystem is the source of truth here too, for the same reason as the
 * artifacts themselves: notes on a page outlive a database reset because the page
 * does.
 *
 * ONE BATCH, ONE TURN. "Send to bough" delivers every unsent note as a SINGLE system
 * message and marks them sent. Per-note delivery would wake a turn per click, which
 * is both expensive and worse feedback — the agent should see the whole review at
 * once, the way a human reviewer's comments arrive together.
 *
 * A corrupt sidecar reads as empty rather than throwing. The page must still render
 * and still accept new notes; failing the artifact because its annotation file has a
 * stray byte trades a real capability for a bookkeeping detail.
 *
 * Ported from `src/server/comments.ts`. Deltas are marked `NOTE:`.
 */
import { z } from "zod";
import { dirname, join, resolve } from "node:path";
import { postSystemNote } from "../agents/notes.ts";
import { NotFoundError, PathError } from "../errors.ts";
import { commentsDir, commentsPathFor, confine } from "../paths.ts";
import { PostCommentBody, SendCommentsBody } from "../schema/requests.ts";
import type { Handler } from "./app.ts";
import { json, parseBody } from "./app.ts";

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/**
 * Where the user clicked.
 *
 * Both halves earn their place: `label` is the human-meaningful part — it is what
 * makes "(near \"Files touched\") this list is stale" a note the agent can act on
 * without opening anything — and `selector`/`xf`/`yf` are how the page re-places the
 * pin on reload. Every field is bounded, because this crosses an HTTP boundary from
 * a page whose own scripts can post to it.
 *
 * NOTE: `schema/requests.ts` (frozen) types the wire `anchor` as `unknown`, so the
 * real shape is validated HERE, where the storage format is owned. A note with an
 * unusable anchor still stores — the text is the point, the pin is the affordance —
 * so parsing falls back to a centered anchor rather than rejecting the note.
 */
export const CommentAnchor = z.object({
  /** Visible text under the click, or the tag name. */
  label: z.string().max(200).default(""),
  /** Best-effort CSS path, for pin placement only. */
  selector: z.string().max(400).default(""),
  /** Click position as a fraction of the full document. */
  xf: z.number().min(0).max(1).default(0.5),
  yf: z.number().min(0).max(1).default(0.5),
});
export type CommentAnchor = z.infer<typeof CommentAnchor>;

export const ArtifactComment = z.object({
  id: z.string(),
  /** The artifact this note is on, as a session-relative name. */
  artifact: z.string(),
  text: z.string().max(4000),
  anchor: CommentAnchor,
  ts: z.number(),
  /** True once delivered to the agent by "Send to bough". */
  sent: z.boolean().default(false),
});
export type ArtifactComment = z.infer<typeof ArtifactComment>;

/** Where the sidecars live. Injected so tests get a hermetic directory. */
export interface CommentStoreOptions {
  /** The comments directory. Absent = `~/.bough/comments` (`paths.ts`). */
  dir?: string;
  /** Injected clock. Absent = `Date.now`. */
  now?: () => number;
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/**
 * The sidecar path for a session, confined to the comments directory.
 *
 * The session id reaches this from a URL, so it is confined exactly like an artifact
 * name is (`artifacts.ts`): a `../` id must not be able to make the server write a
 * JSON file wherever it likes. The single-segment check rejects a descending id for
 * the same reason it does there.
 */
export function commentsPath(sessionId: string, opts: CommentStoreOptions = {}): string {
  if (!sessionId) throw new PathError("comment session id is empty.");
  const dir = resolve(opts.dir ?? commentsDir());
  // The default path comes from `paths.ts` so the layout stays declared in one
  // place; `confine` then judges it, which is what catches a `..` id before it can
  // steer the write out of the store.
  const candidate = opts.dir ? join(dir, `${sessionId}.json`) : commentsPathFor(sessionId);
  const full = confine(dir, candidate);
  if (dirname(full) !== dir) {
    throw new PathError(
      `comment session id must be one path segment: ${JSON.stringify(sessionId)} resolves ` +
        `to ${full}, which is not directly under ${dir}.`,
    );
  }
  return full;
}

/**
 * This session's notes, oldest first. Absent or unreadable → `[]`.
 *
 * Deliberately total: every caller is either rendering a page or answering a widget
 * fetch, and neither has anything useful to do with an exception.
 */
export function loadComments(sessionId: string, opts: CommentStoreOptions = {}): ArtifactComment[] {
  let raw: string;
  try {
    raw = Deno.readTextFileSync(commentsPath(sessionId, opts));
  } catch {
    return [];
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(raw);
  } catch {
    return []; // a corrupt sidecar reads as empty rather than breaking the page
  }
  const parsed = z.array(ArtifactComment).safeParse(decoded);
  return parsed.success ? parsed.data : [];
}

function saveComments(
  sessionId: string,
  comments: ArtifactComment[],
  opts: CommentStoreOptions = {},
): void {
  const path = commentsPath(sessionId, opts);
  Deno.mkdirSync(dirname(path), { recursive: true });
  Deno.writeTextFileSync(path, JSON.stringify(comments, null, 2));
}

/** Add one note. Returns the stored comment, id and timestamp included. */
export function addComment(
  sessionId: string,
  input: { artifact: string; text: string; anchor?: unknown },
  opts: CommentStoreOptions = {},
): ArtifactComment {
  const anchor = CommentAnchor.safeParse(input.anchor ?? {});
  const comment: ArtifactComment = {
    id: crypto.randomUUID(),
    artifact: input.artifact,
    text: input.text.slice(0, 4000),
    anchor: anchor.success ? anchor.data : CommentAnchor.parse({}),
    ts: (opts.now ?? Date.now)(),
    sent: false,
  };
  const comments = loadComments(sessionId, opts);
  comments.push(comment);
  saveComments(sessionId, comments, opts);
  return comment;
}

/** Remove one note. `false` when there was nothing with that id. */
export function deleteComment(
  sessionId: string,
  id: string,
  opts: CommentStoreOptions = {},
): boolean {
  const comments = loadComments(sessionId, opts);
  const next = comments.filter((c) => c.id !== id);
  if (next.length === comments.length) return false;
  saveComments(sessionId, next, opts);
  return true;
}

/**
 * Mark notes delivered.
 *
 * Called only AFTER the system note has landed, so a failure between the two leaves
 * the batch unsent and re-sendable rather than silently swallowed.
 */
export function markSent(sessionId: string, ids: string[], opts: CommentStoreOptions = {}): void {
  const set = new Set(ids);
  const comments = loadComments(sessionId, opts);
  let touched = false;
  for (const c of comments) {
    if (set.has(c.id) && !c.sent) {
      c.sent = true;
      touched = true;
    }
  }
  if (touched) saveComments(sessionId, comments, opts);
}

// ---------------------------------------------------------------------------
// The system note
// ---------------------------------------------------------------------------

/** The prefix the UI keys off to render the batch as review feedback. */
export const COMMENTS_NOTE_PREFIX = "[artifact comments]";

/**
 * The message the agent reads.
 *
 * Grouped BY ARTIFACT and numbered, because a batch spanning two pages read as one
 * flat list makes the agent guess which page each note belongs to. The `(near "…")`
 * clause is the anchor's whole purpose: it is what turns a pin into an instruction.
 * The closing line states the two acceptable moves, so a note the agent disagrees
 * with produces a question rather than silence.
 */
export function formatForAgent(comments: ArtifactComment[]): string {
  const byArtifact = new Map<string, ArtifactComment[]>();
  for (const c of comments) {
    byArtifact.set(c.artifact, [...(byArtifact.get(c.artifact) ?? []), c]);
  }
  const blocks: string[] = [];
  for (const [artifact, list] of byArtifact) {
    const lines = list.map((c, i) => {
      const where = c.anchor.label ? ` (near "${c.anchor.label}")` : "";
      return `  ${i + 1}.${where} ${c.text}`;
    });
    blocks.push(`On the artifact "${artifact}":\n${lines.join("\n")}`);
  }
  const n = comments.length;
  return `${COMMENTS_NOTE_PREFIX} The user left ${n} comment${n === 1 ? "" : "s"} on the ` +
    `artifact${byArtifact.size === 1 ? "" : "s"} for you to read and act on:\n\n` +
    `${blocks.join("\n\n")}\n\nAddress the comments, or reply with questions.`;
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/** `GET /sessions/:id/comments[?artifact=]` — the notes the injected widget renders. */
export const listCommentsH: Handler = (req, _ctx, params) => {
  const artifact = new URL(req.url).searchParams.get("artifact");
  const all = loadComments(params.id);
  return json({ comments: artifact ? all.filter((c) => c.artifact === artifact) : all });
};

/** `POST /sessions/:id/comments` — the widget pins one note. */
export const postCommentH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) {
    throw new NotFoundError(
      `no session ${params.id} — a comment belongs to the session that published the ` +
        `artifact, and that session no longer exists.`,
    );
  }
  const body = await parseBody(req, PostCommentBody);
  return json(addComment(params.id, body), 201);
};

/** `DELETE /sessions/:id/comments/:cid` — the widget removes one note. */
export const deleteCommentH: Handler = (_req, _ctx, params) => {
  if (!deleteComment(params.id, params.cid)) {
    throw new NotFoundError(
      `no comment ${params.cid} in session ${params.id} — it may already have been deleted.`,
    );
  }
  return json({ ok: true });
};

/**
 * `POST /sessions/:id/comments/send` — deliver the batch.
 *
 * Ordering is load-bearing: post the note FIRST, mark sent SECOND. `postSystemNote`
 * owns the wake rule (`agents/notes.ts`) — a turn starts on an idle session and the
 * note rides the queued drain on a busy one, never a second concurrent turn — so
 * nothing about waking is decided here. An empty batch is a 200 with `{sent: 0}`
 * rather than an error: clicking send twice is a no-op, not a failure.
 */
export const sendCommentsH: Handler = async (req, ctx, params) => {
  if (!ctx.db.getSession(params.id)) {
    throw new NotFoundError(
      `no session ${params.id} — there is nothing to deliver these comments to.`,
    );
  }
  const body = await parseBody(req, SendCommentsBody, {});
  const wanted = body.ids ? new Set(body.ids) : null;
  const unsent = loadComments(params.id)
    .filter((c) => !c.sent && (wanted === null || wanted.has(c.id)));
  if (unsent.length === 0) return json({ sent: 0 });

  const delivery = postSystemNote(ctx, params.id, formatForAgent(unsent));
  markSent(params.id, unsent.map((c) => c.id));
  return json({ sent: unsent.length, wake: delivery.wake });
};

// ---------------------------------------------------------------------------
// The injected widget
// ---------------------------------------------------------------------------

/**
 * The comment layer spliced into every served HTML artifact (`artifacts.ts`).
 *
 * Self-contained inline CSS and JS, talking to the SAME origin — which is what makes
 * it work at all: no CORS, and the page can `fetch("/sessions/…/comments")` directly.
 * It derives the session id and artifact name from its own URL rather than being
 * templated with them, so this function interpolates NOTHING and cannot inject
 * anything into the artifact.
 *
 * Kept deliberately small and un-opinionated so it never fights the artifact's own
 * styling: scoped `bgh-` ids, a high z-index, and no global resets.
 */
export function commentWidget(): string {
  return `
<style id="bgh-cmt-style">
#bgh-cmt,#bgh-cmt *{box-sizing:border-box;font-family:system-ui,-apple-system,sans-serif}
#bgh-cmt{position:fixed;right:16px;bottom:16px;z-index:2147483000;display:flex;gap:8px;align-items:center}
#bgh-cmt button{font:13px system-ui;border:0;border-radius:8px;padding:8px 12px;cursor:pointer;
  box-shadow:0 2px 8px rgba(0,0,0,.25)}
#bgh-cmt-toggle{background:#7ec699;color:#12151a}
#bgh-cmt-toggle.on{background:#e0a458}
#bgh-cmt-send{background:#5b8def;color:#fff}
#bgh-cmt-send[hidden]{display:none}
.bgh-pin{position:absolute;z-index:2147482000;width:22px;height:22px;margin:-11px 0 0 -11px;
  border-radius:50% 50% 50% 0;transform:rotate(-45deg);background:#e0a458;border:2px solid #fff;
  box-shadow:0 1px 4px rgba(0,0,0,.4);cursor:pointer;display:flex;align-items:center;justify-content:center}
.bgh-pin.sent{background:#7ec699}
.bgh-pin span{transform:rotate(45deg);color:#12151a;font:bold 11px system-ui}
.bgh-pop{position:absolute;z-index:2147483100;width:260px;background:#1e2126;color:#e5e9ef;
  border:1px solid #3a3f46;border-radius:10px;padding:10px;box-shadow:0 6px 24px rgba(0,0,0,.5);font:13px system-ui}
.bgh-pop textarea{width:100%;min-height:60px;background:#12151a;color:#e5e9ef;border:1px solid #3a3f46;
  border-radius:6px;padding:6px;font:13px system-ui;resize:vertical}
.bgh-pop .bgh-row{display:flex;gap:6px;justify-content:flex-end;margin-top:8px}
.bgh-pop button{font:12px system-ui;border:0;border-radius:6px;padding:5px 10px;cursor:pointer}
.bgh-save{background:#7ec699;color:#12151a}.bgh-del{background:#c96a6a;color:#fff}.bgh-cancel{background:#3a3f46;color:#e5e9ef}
.bgh-cmt-mode,.bgh-cmt-mode *{cursor:crosshair !important}
#bgh-cmt-toast{position:fixed;left:50%;bottom:64px;transform:translateX(-50%);z-index:2147483200;
  background:#12151a;color:#e5e9ef;border:1px solid #3a3f46;border-radius:8px;padding:8px 14px;font:13px system-ui;
  box-shadow:0 4px 16px rgba(0,0,0,.4);opacity:0;transition:opacity .2s}
#bgh-cmt-toast.show{opacity:1}
</style>
<div id="bgh-cmt">
  <button id="bgh-cmt-send" hidden>Send to bough</button>
  <button id="bgh-cmt-toggle" title="Leave a comment for bough">Comment</button>
</div>
<div id="bgh-cmt-toast"></div>
<script id="bgh-cmt-script">
(function(){
  var m = location.pathname.match(/^\\/artifacts\\/([^\\/]+)\\/(.+)$/);
  if(!m) return;
  var sid = m[1], artifact = decodeURIComponent(m[2]);
  var api = "/sessions/"+encodeURIComponent(decodeURIComponent(sid))+"/comments";
  var mode = false, comments = [], pending = null, placed = false, toastTimer = null;
  var toggle = document.getElementById("bgh-cmt-toggle");
  var sendBtn = document.getElementById("bgh-cmt-send");
  var toastEl = document.getElementById("bgh-cmt-toast");

  function toast(t,ms){ toastEl.textContent=t; toastEl.classList.add("show");
    clearTimeout(toastTimer); toastTimer=setTimeout(function(){toastEl.classList.remove("show");},ms||1800); }
  function hideToast(){ clearTimeout(toastTimer); toastEl.classList.remove("show"); }
  function docW(){ return Math.max(document.documentElement.scrollWidth, document.body.scrollWidth); }
  function docH(){ return Math.max(document.documentElement.scrollHeight, document.body.scrollHeight); }

  function anchorFor(e){
    var el = e.target, label = ((el.textContent||el.tagName||"").trim().replace(/\\s+/g," ")).slice(0,80);
    var parts=[], n=el, hops=0;
    while(n && n.nodeType===1 && n.tagName!=="BODY" && n.id!=="bgh-cmt" && hops<4){
      var sel=n.tagName.toLowerCase();
      if(n.id){ parts.unshift(sel+"#"+n.id); break; }
      var sibs=[].slice.call(n.parentNode?n.parentNode.children:[]).filter(function(c){return c.tagName===n.tagName;});
      if(sibs.length>1) sel+=":nth-of-type("+(sibs.indexOf(n)+1)+")";
      parts.unshift(sel); n=n.parentElement; hops++;
    }
    return { label:label, selector:parts.join(" > "), xf:e.pageX/docW(), yf:e.pageY/docH() };
  }

  function setMode(on){
    mode=on; toggle.classList.toggle("on",on); toggle.textContent=on?"Done":"Comment";
    document.body.classList.toggle("bgh-cmt-mode",on);
    // The chip alone doesn't say what comment mode IS — until the first note is
    // placed, activating it explains the one move it wants.
    if(on && !placed) toast("Click anywhere on the page to leave a note for bough.", 4000);
    if(!on){ closePop(); hideToast(); }
  }
  toggle.addEventListener("click",function(){ setMode(!mode); });

  document.addEventListener("click",function(e){
    if(!mode) return;
    if(e.target.closest("#bgh-cmt")||e.target.closest(".bgh-pop")||e.target.closest(".bgh-pin")) return;
    e.preventDefault(); e.stopPropagation();
    openEditor(anchorFor(e), e.pageX, e.pageY);
  }, true);

  function closePop(){ if(pending){ pending.remove(); pending=null; } }

  function openEditor(anchor, x, y){
    closePop(); placed=true; hideToast();
    var pop=document.createElement("div"); pop.className="bgh-pop";
    pop.style.left=Math.min(x, docW()-280)+"px"; pop.style.top=(y+8)+"px";
    pop.innerHTML='<textarea placeholder="Note for bough..."></textarea>'+
      '<div class="bgh-row"><button class="bgh-cancel">Cancel</button><button class="bgh-save">Save</button></div>';
    document.body.appendChild(pop); pending=pop;
    var ta=pop.querySelector("textarea"); ta.focus();
    pop.querySelector(".bgh-cancel").addEventListener("click",closePop);
    pop.querySelector(".bgh-save").addEventListener("click",function(){
      var text=ta.value.trim(); if(!text) return;
      fetch(api,{method:"POST",headers:{"content-type":"application/json"},
        body:JSON.stringify({artifact:artifact,text:text,anchor:anchor})})
        .then(function(r){return r.ok?r.json():Promise.reject();})
        .then(function(){ closePop(); load(); toast("comment saved"); })
        .catch(function(){ toast("couldn't save — is bough running?"); });
    });
  }

  function pinFor(c){
    var pin=document.createElement("div"); pin.className="bgh-pin"+(c.sent?" sent":"");
    pin.style.left=(c.anchor.xf*docW())+"px"; pin.style.top=(c.anchor.yf*docH())+"px";
    pin.innerHTML="<span>"+(c.sent?"\\u2713":"\\u2022")+"</span>";
    pin.addEventListener("click",function(ev){ ev.stopPropagation(); showComment(c, pin); });
    return pin;
  }

  function showComment(c, pin){
    closePop();
    var r=pin.getBoundingClientRect();
    var pop=document.createElement("div"); pop.className="bgh-pop";
    pop.style.left=Math.min(r.left+window.scrollX, docW()-280)+"px";
    pop.style.top=(r.bottom+window.scrollY+8)+"px";
    pop.innerHTML='<div style="white-space:pre-wrap">'+escapeHtml(c.text)+'</div>'+
      '<div style="opacity:.6;font-size:11px;margin-top:6px">'+(c.sent?"sent to bough":"not sent yet")+'</div>'+
      '<div class="bgh-row"><button class="bgh-del">Delete</button><button class="bgh-cancel">Close</button></div>';
    document.body.appendChild(pop); pending=pop;
    pop.querySelector(".bgh-cancel").addEventListener("click",closePop);
    pop.querySelector(".bgh-del").addEventListener("click",function(){
      fetch(api+"/"+encodeURIComponent(c.id),{method:"DELETE"})
        .then(function(){ closePop(); load(); toast("deleted"); });
    });
  }

  function escapeHtml(s){ return s.replace(/[&<>]/g,function(ch){return {"&":"&amp;","<":"&lt;",">":"&gt;"}[ch];}); }

  function render(){
    [].slice.call(document.querySelectorAll(".bgh-pin")).forEach(function(p){p.remove();});
    comments.filter(function(c){return c.artifact===artifact;}).forEach(function(c){
      document.body.appendChild(pinFor(c));
    });
    var unsent=comments.filter(function(c){return c.artifact===artifact && !c.sent;}).length;
    sendBtn.hidden = unsent===0;
    sendBtn.textContent = "Send "+unsent+" to bough";
  }

  function load(){
    fetch(api+"?artifact="+encodeURIComponent(artifact))
      .then(function(r){return r.ok?r.json():{comments:[]};})
      .then(function(d){ comments=d.comments||[]; render(); })
      .catch(function(){});
  }

  sendBtn.addEventListener("click",function(){
    fetch(api+"/send",{method:"POST",headers:{"content-type":"application/json"},body:"{}"})
      .then(function(r){return r.ok?r.json():Promise.reject();})
      .then(function(d){ toast("sent "+(d.sent||0)+" to bough"); load(); })
      .catch(function(){ toast("couldn't reach bough"); });
  });

  window.addEventListener("resize",render);
  load();
})();
</script>
`;
}
