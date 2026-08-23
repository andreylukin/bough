//! Artifact comments (port of `src/server/comments.ts`) — the user's margin
//! notes on a published page, left for the agent to read.
//!
//! WHY THIS EXISTS. An artifact is where the agent's work becomes something a
//! human can actually look at: a diff review, a comparison table, a chart, a
//! clickable prototype. Feedback on it is inherently positional — "this column
//! is wrong", "this row is stale" — and re-typing "the third card under
//! Findings" into the composer loses exactly the part that made the page worth
//! publishing. So the page itself carries the annotation UI: toggle comment
//! mode, click anywhere to pin a note, send the batch, and it arrives as one
//! `[artifact comments]` system message the agent acts on.
//!
//! THE INVARIANT THIS HOLDS: **the sidecar lives OUTSIDE the artifact
//! directory.** Comments persist as one JSON file per session at
//! `~/.bough/comments/<id>.json` (`paths.rs`), a sibling of
//! `~/.bough/artifacts/` — never inside `~/.bough/artifacts/<id>/`. Put it
//! inside and two things break at once, both silently: `list_artifacts` walks it
//! and shows the user a file they never published, and
//! `GET /artifacts/<id>/comments.json` serves the whole note history to anything
//! that asks for it, including the artifact's own scripts.
//!
//! The filesystem is the source of truth here too, for the same reason as the
//! artifacts themselves: notes on a page outlive a database reset because the
//! page does.
//!
//! ONE BATCH, ONE TURN. "Send to bough" delivers every unsent note as a SINGLE
//! system message and marks them sent. Per-note delivery would wake a turn per
//! click, which is both expensive and worse feedback — the agent should see the
//! whole review at once, the way a human reviewer's comments arrive together.
//!
//! A corrupt sidecar reads as empty rather than failing. The page must still
//! render and still accept new notes; failing the artifact because its
//! annotation file has a stray byte trades a real capability for a bookkeeping
//! detail.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use bough_core::agents::notes::{post_system_note, NoteDeps, WakeOutcome};
use bough_core::errors::BoughError;
use bough_core::paths::{comments_dir, comments_path_for, confine};
use bough_core::schema::requests::{PostCommentBody, SendCommentsBody};
use bough_core::types::Clock;

use crate::http::{handler, json, parse_body, Handler};

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/// Where the user clicked.
///
/// Both halves earn their place: `label` is the human-meaningful part — it is
/// what makes "(near \"Files touched\") this list is stale" a note the agent can
/// act on without opening anything — and `selector`/`xf`/`yf` are how the page
/// re-places the pin on reload. Every field is bounded, because this crosses an
/// HTTP boundary from a page whose own scripts can post to it.
///
/// `schema/requests.rs` (frozen) types the wire `anchor` as free-form JSON, so
/// the real shape is validated HERE, where the storage format is owned. A note
/// with an unusable anchor still stores — the text is the point, the pin is the
/// affordance — so parsing falls back to a centered anchor rather than rejecting
/// the note.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CommentAnchor {
    /// Visible text under the click, or the tag name.
    pub label: String,
    /// Best-effort CSS path, for pin placement only.
    pub selector: String,
    /// Click position as a fraction of the full document.
    pub xf: f64,
    pub yf: f64,
}

impl Default for CommentAnchor {
    fn default() -> Self {
        CommentAnchor {
            label: String::new(),
            selector: String::new(),
            xf: 0.5,
            yf: 0.5,
        }
    }
}

impl CommentAnchor {
    /// zod's `object({...}).default(...)` by hand: an object contributes the
    /// fields it has and defaults the rest; anything out of bounds (or not an
    /// object at all) reads as the centered default, exactly as a failed
    /// `safeParse` does.
    pub fn parse(value: Option<&Value>) -> CommentAnchor {
        let Some(value) = value else {
            return CommentAnchor::default();
        };
        if value.is_null() {
            return CommentAnchor::default();
        }
        let Some(obj) = value.as_object() else {
            return CommentAnchor::default();
        };
        let string = |key: &str, max: usize| -> Option<String> {
            match obj.get(key) {
                None | Some(Value::Null) => Some(String::new()),
                Some(Value::String(s)) if s.chars().count() <= max => Some(s.clone()),
                _ => None,
            }
        };
        let fraction = |key: &str| -> Option<f64> {
            match obj.get(key) {
                None | Some(Value::Null) => Some(0.5),
                Some(v) => v.as_f64().filter(|n| (0.0..=1.0).contains(n)),
            }
        };
        match (
            string("label", 200),
            string("selector", 400),
            fraction("xf"),
            fraction("yf"),
        ) {
            (Some(label), Some(selector), Some(xf), Some(yf)) => CommentAnchor {
                label,
                selector,
                xf,
                yf,
            },
            _ => CommentAnchor::default(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ArtifactComment {
    pub id: String,
    /// The artifact this note is on, as a session-relative name.
    pub artifact: String,
    pub text: String,
    pub anchor: CommentAnchor,
    pub ts: i64,
    /// True once delivered to the agent by "Send to bough".
    #[serde(default)]
    pub sent: bool,
}

/// Where the sidecars live. Injected so tests get a hermetic directory.
#[derive(Clone, Default)]
pub struct CommentStoreOptions {
    /// The comments directory. Absent = `~/.bough/comments` (`paths.rs`).
    pub dir: Option<PathBuf>,
    /// Injected clock. Absent = the system clock.
    pub now: Option<Clock>,
}

/// The longest note the store keeps. Longer text is clipped, never refused —
/// the note is the point.
const TEXT_MAX: usize = 4000;

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// The sidecar path for a session, confined to the comments directory.
///
/// The session id reaches this from a URL, so it is confined exactly like an
/// artifact name is: a `../` id must not be able to make the server write a JSON
/// file wherever it likes. The single-segment check rejects a descending id for
/// the same reason it does there.
pub fn comments_path(session_id: &str, opts: &CommentStoreOptions) -> Result<PathBuf, BoughError> {
    if session_id.is_empty() {
        return Err(BoughError::path("comment session id is empty."));
    }
    let dir: PathBuf = match &opts.dir {
        Some(dir) => dir.clone(),
        None => comments_dir(),
    };
    // The default path comes from `paths.rs` so the layout stays declared in
    // one place; `confine` then judges it, which is what catches a `..` id
    // before it can steer the write out of the store.
    let candidate = match &opts.dir {
        Some(dir) => dir.join(format!("{session_id}.json")),
        None => comments_path_for(session_id),
    };
    let full = confine(&dir, &candidate)?;
    if full.parent() != Some(dir.as_path()) {
        return Err(BoughError::path(format!(
            "comment session id must be one path segment: {} resolves to {}, which is not \
             directly under {}.",
            serde_json::to_string(session_id).unwrap_or_default(),
            full.display(),
            dir.display()
        )));
    }
    Ok(full)
}

/// This session's notes, oldest first. Absent or unreadable → `[]`.
///
/// Deliberately total: every caller is either rendering a page or answering a
/// widget fetch, and neither has anything useful to do with an error.
pub fn load_comments(session_id: &str, opts: &CommentStoreOptions) -> Vec<ArtifactComment> {
    let Ok(path) = comments_path(session_id, opts) else {
        return vec![];
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return vec![];
    };
    // A corrupt sidecar reads as empty rather than breaking the page.
    serde_json::from_str::<Vec<ArtifactComment>>(&raw).unwrap_or_default()
}

fn save_comments(
    session_id: &str,
    comments: &[ArtifactComment],
    opts: &CommentStoreOptions,
) -> Result<(), BoughError> {
    let path = comments_path(session_id, opts)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| BoughError::path(format!("cannot create {}: {e}", parent.display())))?;
    }
    let body = serde_json::to_string_pretty(comments).unwrap_or_else(|_| "[]".to_string());
    std::fs::write(&path, body)
        .map_err(|e| BoughError::path(format!("cannot write {}: {e}", path.display())))
}

/// What a widget posts. `anchor` is free-form on the wire and validated here.
pub struct CommentInput<'a> {
    pub artifact: &'a str,
    pub text: &'a str,
    pub anchor: Option<&'a Value>,
}

/// Add one note. Returns the stored comment, id and timestamp included.
pub fn add_comment(
    session_id: &str,
    input: CommentInput<'_>,
    opts: &CommentStoreOptions,
) -> Result<ArtifactComment, BoughError> {
    let now = opts
        .now
        .clone()
        .unwrap_or_else(bough_core::types::system_clock);
    let comment = ArtifactComment {
        id: Uuid::new_v4().to_string(),
        artifact: input.artifact.to_string(),
        text: input.text.chars().take(TEXT_MAX).collect(),
        anchor: CommentAnchor::parse(input.anchor),
        ts: now(),
        sent: false,
    };
    let mut comments = load_comments(session_id, opts);
    comments.push(comment.clone());
    save_comments(session_id, &comments, opts)?;
    Ok(comment)
}

/// Remove one note. `false` when there was nothing with that id.
pub fn delete_comment(session_id: &str, id: &str, opts: &CommentStoreOptions) -> bool {
    let comments = load_comments(session_id, opts);
    let next: Vec<ArtifactComment> = comments.iter().filter(|c| c.id != id).cloned().collect();
    if next.len() == comments.len() {
        return false;
    }
    save_comments(session_id, &next, opts).is_ok()
}

/// Mark notes delivered.
///
/// Called only AFTER the system note has landed, so a failure between the two
/// leaves the batch unsent and re-sendable rather than silently swallowed.
pub fn mark_sent(session_id: &str, ids: &[String], opts: &CommentStoreOptions) {
    let mut comments = load_comments(session_id, opts);
    let mut touched = false;
    for c in comments.iter_mut() {
        if ids.contains(&c.id) && !c.sent {
            c.sent = true;
            touched = true;
        }
    }
    if touched {
        let _ = save_comments(session_id, &comments, opts);
    }
}

// ---------------------------------------------------------------------------
// The system note
// ---------------------------------------------------------------------------

/// The prefix the UI keys off to render the batch as review feedback.
pub const COMMENTS_NOTE_PREFIX: &str = "[artifact comments]";

/// The message the agent reads.
///
/// Grouped BY ARTIFACT and numbered, because a batch spanning two pages read as
/// one flat list makes the agent guess which page each note belongs to. The
/// `(near "…")` clause is the anchor's whole purpose: it is what turns a pin
/// into an instruction. The closing line states the two acceptable moves, so a
/// note the agent disagrees with produces a question rather than silence.
pub fn format_for_agent(comments: &[ArtifactComment]) -> String {
    // Insertion-ordered grouping (the TS `Map`): the first artifact seen is the
    // first block written.
    let mut by_artifact: Vec<(String, Vec<&ArtifactComment>)> = Vec::new();
    for c in comments {
        match by_artifact.iter_mut().find(|(name, _)| *name == c.artifact) {
            Some((_, list)) => list.push(c),
            None => by_artifact.push((c.artifact.clone(), vec![c])),
        }
    }
    let blocks: Vec<String> = by_artifact
        .iter()
        .map(|(artifact, list)| {
            let lines: Vec<String> = list
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let where_ = if c.anchor.label.is_empty() {
                        String::new()
                    } else {
                        format!(" (near \"{}\")", c.anchor.label)
                    };
                    format!("  {}.{where_} {}", i + 1, c.text)
                })
                .collect();
            format!("On the artifact \"{artifact}\":\n{}", lines.join("\n"))
        })
        .collect();
    let n = comments.len();
    format!(
        "{COMMENTS_NOTE_PREFIX} The user left {n} comment{} on the artifact{} for you to read \
         and act on:\n\n{}\n\nAddress the comments, or reply with questions.",
        if n == 1 { "" } else { "s" },
        if by_artifact.len() == 1 { "" } else { "s" },
        blocks.join("\n\n")
    )
}

fn wake_str(wake: WakeOutcome) -> &'static str {
    match wake {
        WakeOutcome::Started => "started",
        WakeOutcome::Queued => "queued",
        WakeOutcome::Recorded => "recorded",
        WakeOutcome::Dropped => "dropped",
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

fn param(params: &crate::http::Params, key: &str) -> String {
    params.get(key).cloned().unwrap_or_default()
}

fn query_param(uri: &axum::http::Uri, key: &str) -> Option<String> {
    let query = uri.query()?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

/// `decodeURIComponent` for a query value, `+` included (the widget encodes
/// with `encodeURIComponent`, but a browser form would send `+`).
fn percent_decode(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `GET /sessions/:id/comments[?artifact=]` — the notes the injected widget
/// renders.
pub fn list_comments() -> Handler {
    handler(|req, _ctx, params| async move {
        let artifact = query_param(req.uri(), "artifact");
        let all = load_comments(&param(&params, "id"), &CommentStoreOptions::default());
        let comments: Vec<ArtifactComment> = match artifact {
            Some(name) => all.into_iter().filter(|c| c.artifact == name).collect(),
            None => all,
        };
        Ok(json(&serde_json::json!({ "comments": comments }), 200))
    })
}

/// `POST /sessions/:id/comments` — the widget pins one note.
pub fn post_comment() -> Handler {
    handler(|req, ctx, params| async move {
        let id = param(&params, "id");
        if ctx.db.lock().unwrap().get_session(&id)?.is_none() {
            return Err(BoughError::not_found(format!(
                "no session {id} — a comment belongs to the session that published the \
                 artifact, and that session no longer exists."
            )));
        }
        let body: PostCommentBody = parse_body(req, None).await?;
        body.validate()?;
        let comment = add_comment(
            &id,
            CommentInput {
                artifact: &body.artifact,
                text: &body.text,
                anchor: body.anchor.as_ref(),
            },
            &CommentStoreOptions::default(),
        )?;
        Ok(json(&comment, 201))
    })
}

/// `DELETE /sessions/:id/comments/:cid` — the widget removes one note.
pub fn delete_comment_route() -> Handler {
    handler(|_req, _ctx, params| async move {
        let id = param(&params, "id");
        let cid = param(&params, "cid");
        if !delete_comment(&id, &cid, &CommentStoreOptions::default()) {
            return Err(BoughError::not_found(format!(
                "no comment {cid} in session {id} — it may already have been deleted."
            )));
        }
        Ok(json(&serde_json::json!({ "ok": true }), 200))
    })
}

/// `POST /sessions/:id/comments/send` — deliver the batch.
///
/// Ordering is load-bearing: post the note FIRST, mark sent SECOND.
/// `post_system_note` owns the wake rule — a turn starts on an idle session and
/// the note rides the queued drain on a busy one, never a second concurrent turn
/// — so nothing about waking is decided here. An empty batch is a 200 with
/// `{sent: 0}` rather than an error: clicking send twice is a no-op, not a
/// failure.
pub fn send_comments() -> Handler {
    handler(|req, ctx, params| async move {
        let id = param(&params, "id");
        if ctx.db.lock().unwrap().get_session(&id)?.is_none() {
            return Err(BoughError::not_found(format!(
                "no session {id} — there is nothing to deliver these comments to."
            )));
        }
        let body: SendCommentsBody = parse_body(req, Some(serde_json::json!({}))).await?;
        let opts = CommentStoreOptions::default();
        let unsent: Vec<ArtifactComment> = load_comments(&id, &opts)
            .into_iter()
            .filter(|c| !c.sent && body.ids.as_ref().is_none_or(|ids| ids.contains(&c.id)))
            .collect();
        if unsent.is_empty() {
            return Ok(json(&serde_json::json!({ "sent": 0 }), 200));
        }

        let delivery =
            post_system_note(&ctx, &id, &format_for_agent(&unsent), &NoteDeps::default());
        mark_sent(
            &id,
            &unsent.iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
            &opts,
        );
        Ok(json(
            &serde_json::json!({ "sent": unsent.len(), "wake": wake_str(delivery.wake) }),
            200,
        ))
    })
}

// ---------------------------------------------------------------------------
// The injected widget
// ---------------------------------------------------------------------------

/// The comment layer spliced into every served HTML artifact (`artifacts.rs`).
///
/// Self-contained inline CSS and JS, talking to the SAME origin — which is what
/// makes it work at all: no CORS, and the page can
/// `fetch("/sessions/…/comments")` directly. It derives the session id and
/// artifact name from its own URL rather than being templated with them, so this
/// function interpolates NOTHING and cannot inject anything into the artifact.
///
/// Kept deliberately small and un-opinionated so it never fights the artifact's
/// own styling: scoped `bgh-` ids, a high z-index, and no global resets.
pub fn comment_widget() -> &'static str {
    WIDGET
}

/// The widget markup, byte-for-byte what the TS template literal emits.
const WIDGET: &str = r##"
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
  var m = location.pathname.match(/^\/artifacts\/([^\/]+)\/(.+)$/);
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
    var el = e.target, label = ((el.textContent||el.tagName||"").trim().replace(/\s+/g," ")).slice(0,80);
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
"##;

// ---------------------------------------------------------------------------
// Tests (port of `src/server/comments.test.ts`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{create_handler, CreateHandlerOptions, Dispatcher};
    use crate::http::testutil::{self, Fixture};
    use bough_core::hostfn::artifact::{list_artifacts, publish_artifact, ArtifactStoreOptions};
    use bough_core::schema::parts::{Part, Session, SessionKind};
    use serde_json::json as j;
    use std::sync::MutexGuard;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> TmpDir {
            let dir = std::env::temp_dir().join(format!("bough-comments-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            TmpDir(dir)
        }
        fn opts(&self) -> CommentStoreOptions {
            CommentStoreOptions {
                dir: Some(self.0.clone()),
                now: None,
            }
        }
        fn entries(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `BOUGH_HOME` is process-global and the handlers read the default paths
    /// per call, so the env-touching tests serialize on the crate-wide lock and
    /// restore on drop.
    struct HomeGuard {
        _lock: MutexGuard<'static, ()>,
        tmp: TmpDir,
        previous: Option<String>,
    }
    impl HomeGuard {
        fn new() -> HomeGuard {
            let lock = testutil::home_lock();
            let tmp = TmpDir::new();
            let previous = std::env::var("BOUGH_HOME").ok();
            std::env::set_var("BOUGH_HOME", &tmp.0);
            HomeGuard {
                _lock: lock,
                tmp,
                previous,
            }
        }
        fn home(&self) -> PathBuf {
            self.tmp.0.clone()
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var("BOUGH_HOME", v),
                None => std::env::remove_var("BOUGH_HOME"),
            }
        }
    }

    fn anchor() -> Value {
        j!({ "label": "Files touched", "selector": "body > h2", "xf": 0.5, "yf": 0.3 })
    }

    fn input<'a>(artifact: &'a str, text: &'a str, anchor: Option<&'a Value>) -> CommentInput<'a> {
        CommentInput {
            artifact,
            text,
            anchor,
        }
    }

    fn seed_session(fx: &Fixture, id: &str) -> Session {
        fx.ctx
            .db
            .lock()
            .unwrap()
            .create_session(Session {
                id: id.to_string(),
                title: id.to_string(),
                kind: SessionKind::Root,
                created_at: 1_000,
                parent_id: None,
                origin_id: None,
                origin_message_id: None,
                workspace: None,
                origin_dir: None,
                base: None,
                model: None,
                effort: None,
                draft: None,
                context_tokens: None,
                cached_tokens: None,
                last_llm_at: None,
                outcome_ok: None,
                description: None,
            })
            .unwrap()
    }

    fn call(fx: &Fixture) -> Dispatcher {
        create_handler(fx.ctx.clone(), CreateHandlerOptions::default())
    }

    // ---- storage ------------------------------------------------------------

    #[test]
    fn add_comment_persists_load_reads_back_delete_removes() {
        let dir = TmpDir::new();
        let a = anchor();
        let c = add_comment(
            "s1",
            input("index.html", "this list is stale", Some(&a)),
            &dir.opts(),
        )
        .unwrap();
        assert!(!c.id.is_empty());
        assert!(!c.sent);
        assert_eq!(c.anchor.label, "Files touched");
        assert_eq!(load_comments("s1", &dir.opts()).len(), 1);
        assert_eq!(
            load_comments("s1", &dir.opts())[0].text,
            "this list is stale"
        );
        assert!(delete_comment("s1", &c.id, &dir.opts()));
        assert!(load_comments("s1", &dir.opts()).is_empty());
        assert!(!delete_comment("s1", "nope", &dir.opts()));
    }

    #[test]
    fn mark_sent_flips_only_the_named_notes() {
        let dir = TmpDir::new();
        let a = anchor();
        let one = add_comment("s2", input("index.html", "one", Some(&a)), &dir.opts()).unwrap();
        let two = add_comment("s2", input("index.html", "two", Some(&a)), &dir.opts()).unwrap();
        mark_sent("s2", std::slice::from_ref(&one.id), &dir.opts());
        let all = load_comments("s2", &dir.opts());
        assert!(all.iter().find(|c| c.id == one.id).unwrap().sent);
        assert!(!all.iter().find(|c| c.id == two.id).unwrap().sent);
    }

    #[test]
    fn a_corrupt_sidecar_reads_as_empty_rather_than_breaking_the_page() {
        let dir = TmpDir::new();
        std::fs::write(dir.0.join("s3.json"), "{not json at all").unwrap();
        assert!(load_comments("s3", &dir.opts()).is_empty());
        // …and a new note still saves over it, so the page stays usable.
        let a = anchor();
        let c = add_comment("s3", input("x.html", "still works", Some(&a)), &dir.opts()).unwrap();
        assert_eq!(
            load_comments("s3", &dir.opts())
                .iter()
                .map(|x| x.id.clone())
                .collect::<Vec<_>>(),
            vec![c.id]
        );
    }

    #[test]
    fn an_unusable_anchor_stores_a_centered_default_the_text_is_the_point() {
        let dir = TmpDir::new();
        let nonsense = Value::String("nonsense".into());
        let c = add_comment("s4", input("x.html", "note", Some(&nonsense)), &dir.opts()).unwrap();
        assert_eq!(c.anchor, CommentAnchor::default());
        let d = add_comment("s4", input("x.html", "note", None), &dir.opts()).unwrap();
        assert_eq!(d.anchor.xf, 0.5);
        // An out-of-bounds fraction is the same fact: the pin cannot be placed.
        let wild = j!({ "label": "x", "xf": 4.0 });
        let e = add_comment("s4", input("x.html", "note", Some(&wild)), &dir.opts()).unwrap();
        assert_eq!(e.anchor, CommentAnchor::default());
    }

    #[test]
    fn a_traversing_session_id_cannot_steer_the_sidecar_write() {
        let dir = TmpDir::new();
        let outside = TmpDir::new();
        let a = anchor();
        for bad in ["../evil", "../../evil", "a/b", "sub/../../evil", ""] {
            assert!(comments_path(bad, &dir.opts()).is_err(), "id {bad:?}");
            assert!(add_comment(bad, input("x", "t", Some(&a)), &dir.opts()).is_err());
            assert!(load_comments(bad, &dir.opts()).is_empty()); // reads are safe-empty
        }
        let absolute = outside.0.to_string_lossy().into_owned();
        assert!(comments_path(&absolute, &dir.opts()).is_err());
        assert!(dir.entries().is_empty());
        assert!(outside.entries().is_empty());

        // `..` is not an escape here, because the sidecar name is `<id>.json`:
        // it lands on `...json` INSIDE the store. Asserted rather than assumed
        // — the interesting property is that nothing leaves `dir`.
        let odd = add_comment("..", input("x", "t", Some(&a)), &dir.opts()).unwrap();
        assert_eq!(
            comments_path("..", &dir.opts()).unwrap(),
            dir.0.join("...json")
        );
        assert_eq!(
            load_comments("..", &dir.opts())
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
            vec![odd.id]
        );
        assert!(outside.entries().is_empty());
    }

    // ---- AC: the sidecar is not walked by list_artifacts --------------------

    #[test]
    fn ac_the_sidecar_is_outside_the_artifact_tree_and_never_listed() {
        let home = HomeGuard::new();
        publish_artifact(
            "s5",
            "index.html",
            "<h1>hi</h1>",
            &ArtifactStoreOptions::default(),
        )
        .unwrap();
        let a = anchor();
        add_comment(
            "s5",
            input("index.html", "note", Some(&a)),
            &CommentStoreOptions::default(),
        )
        .unwrap();

        let sidecar = comments_path("s5", &CommentStoreOptions::default()).unwrap();
        assert!(sidecar.is_file());
        // A SIBLING of the artifacts tree, never inside it — the whole invariant.
        assert!(!sidecar.starts_with(home.home().join("artifacts")));
        assert_eq!(sidecar, home.home().join("comments").join("s5.json"));

        assert_eq!(
            list_artifacts("s5", &ArtifactStoreOptions::default())
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>(),
            vec!["index.html".to_string()]
        );
    }

    // ---- the agent-facing note ----------------------------------------------

    fn comment(id: &str, artifact: &str, text: &str, label: &str) -> ArtifactComment {
        ArtifactComment {
            id: id.into(),
            artifact: artifact.into(),
            text: text.into(),
            anchor: CommentAnchor {
                label: label.into(),
                ..Default::default()
            },
            ts: 1,
            sent: false,
        }
    }

    #[test]
    fn format_for_agent_groups_by_artifact_and_names_the_anchor() {
        let note = format_for_agent(&[
            comment("1", "index.html", "fix this", "Files touched"),
            comment("2", "chart.html", "wrong axis", ""),
        ]);
        assert!(note.starts_with(COMMENTS_NOTE_PREFIX));
        assert!(note.contains("left 2 comments"));
        assert!(note.contains("On the artifact \"index.html\""));
        assert!(note.contains("On the artifact \"chart.html\""));
        assert!(note.contains("1. (near \"Files touched\") fix this"));
        assert!(
            note.contains("1. wrong axis"),
            "no anchor → no \"(near …)\""
        );
        assert!(note.contains("Address the comments, or reply with questions."));
    }

    #[test]
    fn format_for_agent_stays_singular_for_one_comment_on_one_artifact() {
        let note = format_for_agent(&[comment("1", "a.html", "t", "Files touched")]);
        assert!(note.contains("left 1 comment on the artifact"), "{note}");
    }

    // ---- routes -------------------------------------------------------------

    #[tokio::test]
    async fn post_adds_a_note_get_filters_by_artifact_delete_removes_it() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        seed_session(&fx, "sA");
        let call = call(&fx);

        let created = call
            .call(testutil::req(
                "POST",
                "/sessions/sA/comments",
                Some(j!({ "artifact": "index.html", "text": "stale", "anchor": anchor() })),
            ))
            .await;
        assert_eq!(created.status(), 201);
        let note = testutil::body_json(created).await;

        call.call(testutil::req(
            "POST",
            "/sessions/sA/comments",
            Some(j!({ "artifact": "chart.html", "text": "axis", "anchor": anchor() })),
        ))
        .await;

        let all =
            testutil::body_json(call.call(testutil::get("/sessions/sA/comments")).await).await;
        assert_eq!(all["comments"].as_array().unwrap().len(), 2);

        let filtered = testutil::body_json(
            call.call(testutil::get("/sessions/sA/comments?artifact=chart.html"))
                .await,
        )
        .await;
        assert_eq!(
            filtered["comments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["text"].as_str().unwrap().to_string())
                .collect::<Vec<_>>(),
            vec!["axis".to_string()]
        );

        let id = note["id"].as_str().unwrap();
        let removed = call
            .call(testutil::req(
                "DELETE",
                &format!("/sessions/sA/comments/{id}"),
                None,
            ))
            .await;
        assert_eq!(removed.status(), 200);
        let missing = call
            .call(testutil::req(
                "DELETE",
                &format!("/sessions/sA/comments/{id}"),
                None,
            ))
            .await;
        assert_eq!(missing.status(), 404);
    }

    #[tokio::test]
    async fn posting_a_comment_to_an_unknown_session_is_a_404_not_a_stray_file() {
        let home = HomeGuard::new();
        let fx = testutil::fixture();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions/ghost/comments",
                Some(j!({ "artifact": "index.html", "text": "t", "anchor": anchor() })),
            ))
            .await;
        assert_eq!(res.status(), 404);
        assert!(!home.home().join("comments").join("ghost.json").exists());
    }

    #[tokio::test]
    async fn ac_send_posts_one_system_note_for_the_batch_and_marks_them_sent() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        seed_session(&fx, "sB");
        let a = anchor();
        let opts = CommentStoreOptions::default();
        add_comment("sB", input("index.html", "first", Some(&a)), &opts).unwrap();
        add_comment("sB", input("index.html", "second", Some(&a)), &opts).unwrap();
        add_comment("sB", input("chart.html", "third", Some(&a)), &opts).unwrap();

        let call = call(&fx);
        let res = call
            .call(testutil::req(
                "POST",
                "/sessions/sB/comments/send",
                Some(j!({})),
            ))
            .await;
        assert_eq!(res.status(), 200);
        assert_eq!(testutil::body_json(res).await["sent"], 3);

        // One message, not three — the agent should see the whole review at
        // once. It is persisted on the thread the agent replays, not just
        // announced.
        let messages = fx.ctx.db.lock().unwrap().messages_for("sB").unwrap();
        assert_eq!(messages.len(), 1);
        let message = &messages[0];
        assert_eq!(message.role, bough_core::schema::parts::Role::System);
        assert!(!message.pending);
        let text: String = message
            .parts
            .iter()
            .map(|p| match p {
                Part::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert!(text.starts_with(COMMENTS_NOTE_PREFIX));
        assert!(text.contains("first"));
        assert!(text.contains("third"));

        // Every note is now marked sent, so a second click is a no-op.
        assert!(load_comments("sB", &opts).iter().all(|c| c.sent));
        let again = call
            .call(testutil::req(
                "POST",
                "/sessions/sB/comments/send",
                Some(j!({})),
            ))
            .await;
        assert_eq!(testutil::body_json(again).await["sent"], 0);
        assert_eq!(
            fx.ctx.db.lock().unwrap().messages_for("sB").unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn send_can_deliver_a_named_subset_and_leaves_the_rest_unsent() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        seed_session(&fx, "sC");
        let a = anchor();
        let opts = CommentStoreOptions::default();
        let one = add_comment("sC", input("index.html", "one", Some(&a)), &opts).unwrap();
        add_comment("sC", input("index.html", "two", Some(&a)), &opts).unwrap();

        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions/sC/comments/send",
                Some(j!({ "ids": [one.id] })),
            ))
            .await;
        assert_eq!(testutil::body_json(res).await["sent"], 1);
        let after = load_comments("sC", &opts);
        assert!(after.iter().find(|c| c.id == one.id).unwrap().sent);
        assert_eq!(after.iter().filter(|c| !c.sent).count(), 1);
    }

    #[tokio::test]
    async fn sending_into_an_unknown_session_is_a_404_with_nothing_delivered() {
        let _home = HomeGuard::new();
        let fx = testutil::fixture();
        let res = call(&fx)
            .call(testutil::req(
                "POST",
                "/sessions/ghost/comments/send",
                Some(j!({})),
            ))
            .await;
        assert_eq!(res.status(), 404);
    }

    // ---- the injected widget ------------------------------------------------

    #[test]
    fn the_widget_is_self_contained_no_external_network_references() {
        let w = comment_widget().to_ascii_lowercase();
        assert!(!w.contains("src=\"http") && !w.contains("src='http"));
        assert!(!w.contains("href=\"http") && !w.contains("href='http"));
        for banned in ["cdn.", "googleapis", "unpkg", "jsdelivr", "fonts."] {
            assert!(!w.contains(banned), "{banned}");
        }
        // It talks to the same origin, by relative path only.
        assert!(comment_widget().contains("\"/sessions/\""));
    }

    #[test]
    fn the_widget_interpolates_nothing_it_reads_its_identity_from_location() {
        // Called twice, byte-identical: there is no per-session or per-artifact
        // templating, so the layer cannot inject anything into the page it is
        // spliced into.
        assert_eq!(comment_widget(), comment_widget());
        assert!(comment_widget().contains("location.pathname"));
    }
}
