/**
 * Artifact comments — the user's margin notes on a published artifact, left for
 * the agent to read. Every served HTML artifact gets a comment layer injected at
 * serve time (see `commentWidget` + artifacts.ts): the user toggles comment mode,
 * clicks anywhere to drop an anchored note, and hits "Send to bough" to wake the
 * session with the batch (server/app.ts → postSystemNote). The agent reads them
 * as a normal thread message.
 *
 * Storage is a per-session JSON sidecar at ~/.bough/artifacts/<sessionId>.comments.json
 * — a SIBLING of the session's artifact dir, so it is never walked by listArtifacts
 * or reachable through serveArtifact (both confine to the dir). The filesystem is the
 * source of truth, like the artifacts themselves.
 */
import { z } from "zod";
import { join } from "node:path";
import { artifactsRoot } from "./artifacts.ts";

/** Where the user clicked, captured so the agent knows WHAT a note refers to and
 * the page can re-place the pin on reload. */
export const CommentAnchor = z.object({
  /** Visible text under the click (or the tag name) — the human-meaningful anchor. */
  label: z.string().max(200),
  /** Best-effort CSS path for re-locating the element (pin placement only). */
  selector: z.string().max(400).default(""),
  /** Click position as a fraction of the full document, for the pin marker. */
  xf: z.number().min(0).max(1),
  yf: z.number().min(0).max(1),
});
export type CommentAnchor = z.infer<typeof CommentAnchor>;

export const ArtifactComment = z.object({
  id: z.string(),
  /** The artifact this note is on (session-relative name, e.g. "index.html"). */
  artifact: z.string(),
  text: z.string().max(4000),
  anchor: CommentAnchor,
  ts: z.number(),
  /** True once delivered to the agent via "Send to bough". */
  sent: z.boolean().default(false),
});
export type ArtifactComment = z.infer<typeof ArtifactComment>;

/** POST /sessions/:id/comments body — the widget adds one note. */
export const AddCommentBody = z.object({
  artifact: z.string().min(1).max(400),
  text: z.string().min(1).max(4000),
  anchor: CommentAnchor,
});

function commentsPath(sessionId: string, base?: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(sessionId)) throw new Error(`invalid session id: ${sessionId}`);
  return join(artifactsRoot(base), `${sessionId}.comments.json`);
}

export function loadComments(sessionId: string, base?: string): ArtifactComment[] {
  let raw: string;
  try {
    raw = Deno.readTextFileSync(commentsPath(sessionId, base));
  } catch {
    return [];
  }
  try {
    return z.array(ArtifactComment).parse(JSON.parse(raw));
  } catch {
    return []; // a corrupt sidecar reads as empty rather than breaking the page
  }
}

function saveComments(sessionId: string, comments: ArtifactComment[], base?: string): void {
  const path = commentsPath(sessionId, base);
  Deno.mkdirSync(artifactsRoot(base), { recursive: true });
  Deno.writeTextFileSync(path, JSON.stringify(comments, null, 2));
}

export function addComment(
  sessionId: string,
  input: z.infer<typeof AddCommentBody>,
  base?: string,
): ArtifactComment {
  const comments = loadComments(sessionId, base);
  const comment: ArtifactComment = {
    id: crypto.randomUUID(),
    artifact: input.artifact,
    text: input.text,
    anchor: input.anchor,
    ts: Date.now(),
    sent: false,
  };
  comments.push(comment);
  saveComments(sessionId, comments, base);
  return comment;
}

export function deleteComment(sessionId: string, id: string, base?: string): boolean {
  const comments = loadComments(sessionId, base);
  const next = comments.filter((c) => c.id !== id);
  if (next.length === comments.length) return false;
  saveComments(sessionId, next, base);
  return true;
}

/** Mark the given comments delivered (called after a successful "Send to bough"). */
export function markSent(sessionId: string, ids: string[], base?: string): void {
  const set = new Set(ids);
  const comments = loadComments(sessionId, base);
  for (const c of comments) if (set.has(c.id)) c.sent = true;
  saveComments(sessionId, comments, base);
}

/** The system note that wakes the session — the agent reads this as a message. */
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
  return `[artifact comments] The user left ${n} comment${n === 1 ? "" : "s"} on the ` +
    `artifact${byArtifact.size === 1 ? "" : "s"} for you to read and act on:\n\n` +
    `${blocks.join("\n\n")}\n\nAddress the comments, or reply with questions.`;
}

// ---- injected widget --------------------------------------------------------

/**
 * The comment layer injected into every served HTML artifact (artifacts.ts).
 * Self-contained inline CSS+JS; talks to the SAME origin (the bough server), so
 * no CORS and the login cookie rides along. Derives the session id + artifact
 * name from its own URL. Kept deliberately small and un-opinionated so it never
 * fights the artifact's own styling (scoped ids, high z-index, reset-guarded).
 */
export function commentWidget(): string {
  // NOTE: no external deps, no template interpolation of untrusted data — the
  // script reads everything it needs from location at runtime.
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
<div id="bgh-cmt" data-html2canvas-ignore>
  <button id="bgh-cmt-send" hidden>Send to bough</button>
  <button id="bgh-cmt-toggle" title="Leave a comment for bough">💬 Comment</button>
</div>
<div id="bgh-cmt-toast"></div>
<script id="bgh-cmt-script">
(function(){
  var m = location.pathname.match(/^\\/artifacts\\/([^\\/]+)\\/(.+)$/);
  if(!m) return;
  var sid = m[1], artifact = decodeURIComponent(m[2]);
  var api = "/sessions/"+encodeURIComponent(sid)+"/comments";
  var mode = false, comments = [], pending = null;
  var toggle = document.getElementById("bgh-cmt-toggle");
  var sendBtn = document.getElementById("bgh-cmt-send");
  var toastEl = document.getElementById("bgh-cmt-toast");

  var toastTimer=null, placed=false;
  function toast(t,ms){ toastEl.textContent=t; toastEl.classList.add("show"); clearTimeout(toastTimer); toastTimer=setTimeout(function(){toastEl.classList.remove("show");},ms||1800); }
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
    mode=on; toggle.classList.toggle("on",on); toggle.textContent=on?"✓ Done":"💬 Comment";
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
    pop.innerHTML='<textarea placeholder="Note for bough…"></textarea>'+
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
    pin.innerHTML="<span>"+(c.sent?"✓":"•")+"</span>";
    pin.addEventListener("click",function(ev){ ev.stopPropagation(); showComment(c, pin); });
    return pin;
  }

  function showComment(c, pin){
    closePop();
    var r=pin.getBoundingClientRect();
    var pop=document.createElement("div"); pop.className="bgh-pop";
    pop.style.left=Math.min(r.left+window.scrollX, docW()-280)+"px";
    pop.style.top=(r.bottom+window.scrollY+8)+"px";
    var meta=(c.sent?"sent to bough":"not sent yet");
    pop.innerHTML='<div style="white-space:pre-wrap">'+escapeHtml(c.text)+'</div>'+
      '<div style="opacity:.6;font-size:11px;margin-top:6px">'+meta+'</div>'+
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
    fetch(api+"/send",{method:"POST",headers:{"content-type":"application/json"},
      body:JSON.stringify({artifact:artifact})})
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
