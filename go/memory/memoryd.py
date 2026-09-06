"""memoryd: bough's local memory. Drawer, index, reader.

Drawer   every chunk the agent saw, verbatim, in SQLite (~/.bough/memory/memory.db),
         addressed by session and seq. Never summarised away.
Index    FTS5 (BM25) plus a static embedding (model2vec) over the full text of every
         chunk, fused by reciprocal rank. Milliseconds, no model, all sessions.
Reader   a small local model (Granite 4.0 H-Tiny by default) that reads the top hits
         and answers {"seq", "quote", "answer"}. The quote and the answer must occur
         verbatim in that chunk or the answer is dropped. It never answers from its
         own memory, so it has nothing to fabricate from.
Ledger   per-turn records (decisions, facts, failures, files) the reader extracts
         from a turn's chunks, indexed like chunks with kind "ledger", so
         cross-session questions find them first.

Single-threaded on purpose: MLX streams are thread-local.

  POST /index       {session, seq, kind, text}       -> {line}
  POST /search      {query, session?, k?, kinds?}     -> {hits: [{session, seq, kind, line, score}]}
  POST /recall      {question, session?, k?}          -> {answer, seq, session, quote, verified}
  POST /note        {request, session, k?}            -> {facts: [{seq, quote, fact}]}
  POST /consolidate {session, from_seq, to_seq}       -> {records}
  GET  /lines?session=S                               -> {lines: {seq: line}}
  GET  /status
"""

import json
import os
import re
import sqlite3
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse

import numpy as np
import mlx.core as mx
from mlx_lm import load
from mlx_lm.generate import generate_step
from mlx_lm.models.cache import make_prompt_cache
from model2vec import StaticModel

READER = os.environ.get("BOUGH_MEMORY_READER", "mlx-community/granite-4.0-h-tiny-4bit")
EMBED = os.environ.get("BOUGH_MEMORY_EMBED", "minishlab/potion-base-8M")
DB = os.path.expanduser(os.environ.get("BOUGH_MEMORY_DB", "~/.bough/memory/memory.db"))
PORT = int(os.environ.get("BOUGH_MEMORY_PORT", "8765"))
CHUNK_READ = 3000   # chars of one chunk the reader sees
LINE_MIN = 800      # chunks under this get their first line as the index line, no model call

model = tok = emb = None
db = None
vecs = {}  # (session, seq) -> unit vector


def boot():
    global model, tok, emb, db
    t = time.time()
    model, tok = load(READER)
    emb = StaticModel.from_pretrained(EMBED)
    os.makedirs(os.path.dirname(DB), exist_ok=True)
    db = sqlite3.connect(DB)
    db.executescript("""
      create table if not exists chunks(session text, seq integer, kind text, text text, line text, ts real, primary key(session, seq));
      create virtual table if not exists chunks_fts using fts5(text, line, content='chunks', content_rowid='rowid');
      create table if not exists vec(session text, seq integer, v blob, primary key(session, seq));
      create table if not exists ledger(session text, seq integer, kind text, fact text, quote text, ts real);
    """)
    for s, q, v in db.execute("select session, seq, v from vec"):
        vecs[(s, q)] = np.frombuffer(v, dtype=np.float32)
    print(f"memoryd: {READER} + {EMBED} loaded in {time.time()-t:.0f}s; {len(vecs)} chunks indexed", flush=True)


def chat(system, user, max_tokens=120):
    prompt = tok.apply_chat_template([{"role": "system", "content": system}, {"role": "user", "content": user}], tokenize=True, add_generation_prompt=True)
    c = make_prompt_cache(model)
    o = []
    for token, _ in generate_step(mx.array(prompt), model, prompt_cache=c, max_tokens=max_tokens):
        if token in tok.eos_token_ids:
            break
        o.append(token)
    mx.clear_cache()
    return tok.decode(o).strip()


def embed(text):
    v = emb.encode([text[:4000]])[0].astype(np.float32)
    return v / (np.linalg.norm(v) + 1e-9)


# ---------------- index ----------------
LINE_SYS = "You write one-line index entries for tool outputs in a coding agent's history. Reply with ONE line, at most 120 characters: what the output is (which command, file or query) and the facts in it a later step could need (counts, paths, errors, key values). No preamble."

def index(session, seq, kind, text):
    if db.execute("select 1 from chunks where session=? and seq=?", (session, seq)).fetchone():
        line = db.execute("select line from chunks where session=? and seq=?", (session, seq)).fetchone()[0]
        return {"line": line, "skipped": True}
    line = text.strip().split("\n")[0][:120]
    if kind == "tool output" and len(text) >= LINE_MIN:
        body = text if len(text) <= 6000 else text[:3000] + "\n…\n" + text[-3000:]
        line = chat(LINE_SYS, body, 60).split("\n")[0][:160] or line
    cur = db.execute("insert into chunks values (?,?,?,?,?,?)", (session, seq, kind, text, line, time.time()))
    db.execute("insert into chunks_fts(rowid, text, line) values (?,?,?)", (cur.lastrowid, text, line))
    v = embed(f"[{kind}] {line}\n{text}")
    db.execute("insert or replace into vec values (?,?,?)", (session, seq, v.tobytes()))
    vecs[(session, seq)] = v
    db.commit()
    return {"line": line}


def lines(session):
    return {"lines": {str(q): l for q, l in db.execute("select seq, line from chunks where session=? and kind!='ledger'", (session,))}}


# ---------------- search ----------------
def fts_query(q):
    terms = [t for t in re.findall(r"[A-Za-z0-9_./-]{2,}", q)]
    return " OR ".join('"' + t.replace('"', "") + '"' for t in terms[:32])

def search(query, session=None, k=8, kinds=None):
    where = []; args = []
    if session:
        where.append("c.session=?"); args.append(session)
    if kinds:
        where.append("c.kind in (%s)" % ",".join("?" * len(kinds))); args += kinds
    w = (" and " + " and ".join(where)) if where else ""
    bm = []
    fq = fts_query(query)
    if fq:
        rows = db.execute(f"select c.session, c.seq from chunks_fts f join chunks c on c.rowid=f.rowid where chunks_fts match ?{w} order by bm25(chunks_fts) limit 60", [fq] + args).fetchall()
        bm = [tuple(r) for r in rows]
    qv = embed(query)
    keys = [key for key in vecs if (not session or key[0] == session)]
    if kinds:
        allowed = {tuple(r) for r in db.execute(f"select session, seq from chunks c where 1=1{w}", args)}
        keys = [key for key in keys if key in allowed]
    vs = []
    if keys:
        M = np.stack([vecs[key] for key in keys]); sims = M @ qv
        vs = [keys[i] for i in np.argsort(-sims)[:60]]
    score = {}
    for lst in (bm, vs):
        for i, key in enumerate(lst):
            score[key] = score.get(key, 0.0) + 1.0 / (60 + i)
    top = sorted(score.items(), key=lambda x: -x[1])[:k]
    hits = []
    for (s, q), sc in top:
        kind, line = db.execute("select kind, line from chunks where session=? and seq=?", (s, q)).fetchone()
        hits.append({"session": s, "seq": q, "kind": kind, "line": line, "score": round(sc, 4)})
    return {"hits": hits}


# ---------------- reader ----------------
READ_SYS = ("You answer a question from the chunks below, each headed [#SEQ session kind]. Answer in one or two sentences, copying the exact value (number, name, path, identifier, error) verbatim from the chunk that contains it, and name that chunk as [#SEQ]. "
            "If none of the chunks contain the answer, reply exactly: not found.")

def chunk_text(session, seq):
    r = db.execute("select text from chunks where session=? and seq=?", (session, seq)).fetchone()
    return r[0] if r else ""

def parse_obj(s):
    m = re.search(r"\{.*?\}", s, re.S)
    if not m:
        return None
    try:
        return json.loads(m.group(0))
    except Exception:
        try:
            return json.loads(m.group(0).replace("'", '"').replace(",}", "}"))
        except Exception:
            return None

STOP = {"the", "and", "that", "this", "with", "from", "for", "was", "were", "are", "has", "have", "not", "found", "chunk", "chunks", "answer", "question", "paper", "output", "which", "into", "than", "then", "there", "their", "about", "session", "value", "values"}

def claims(reply, question):
    """The values a prose answer asserts, most specific first: quoted or
    backticked spans, then tokens with digits, identifiers, or capitals,
    excluding what the question itself supplied."""
    qtok = {t.lower() for t in re.findall(r"[A-Za-z0-9_./:-]{2,}", question)}
    spans = [x.strip() for grp in re.findall(r"`([^`]{2,80})`|\"([^\"]{2,80})\"|\u201c([^\u201d]{2,80})\u201d", reply) for x in grp if x.strip()]
    toks = []
    for t in re.findall(r"[A-Za-z0-9_][A-Za-z0-9_./:+-]{1,}", reply):
        tl = t.lower().strip(".,;:")
        if tl in qtok or tl in STOP or len(tl) < 2:
            continue
        if re.search(r"\d", t) or re.search(r"[_./:-]", t) or re.search(r"[A-Z]", t[1:]) or (t[0].isupper() and len(t) >= 4):
            toks.append(t.strip(".,;:"))
    seen = set(); out = []
    for v in spans + toks:
        if v.lower() not in seen:
            seen.add(v.lower()); out.append(v)
    return out[:20]

def verify(session, seq, quote, answer):
    if seq is None or not quote or answer is None:
        return False
    low = chunk_text(session, seq).lower()
    return quote.strip().lower() in low and str(answer).strip().lower() in low

# What recall and note may read: never the conversation's own prompts,
# replies or code, which would let a question "verify" itself.
EVIDENCE = ["tool output", "background job", "ledger"]

def recall(question, session=None, k=8):
    hits = search(question, session, k, EVIDENCE)["hits"]
    if not hits:
        return {"answer": None, "verified": False, "hits": []}
    body = "".join(f"\n[#{h['seq']} {h['session']} {h['kind']}]\n{chunk_text(h['session'], h['seq'])[:CHUNK_READ]}\n" for h in hits)
    reply = chat(READ_SYS, body + "\n\nQuestion: " + question, 160)
    ids = [f"{h['session']}#{h['seq']}" for h in hits]
    if "not found" in reply.lower()[:40] or not reply.strip():
        return {"answer": None, "verified": False, "raw": None, "hits": ids}
    # verify: every value the answer asserts must occur verbatim in a hit chunk;
    # the chunk it names is tried first, then the others.
    named = [int(x) for x in re.findall(r"\[#(\d+)\]", reply)]
    order = sorted(hits, key=lambda h: 0 if h["seq"] in named else 1)
    texts = {h["seq"]: chunk_text(h["session"], h["seq"]) for h in order}
    vals = claims(re.sub(r"\[#\d+\]", "", reply), question)
    if not vals:
        return {"answer": None, "verified": False, "raw": reply[:200], "hits": ids}
    where = None; found = []; missing = []
    for v in vals:
        hit = next((h for h in order if v.lower() in texts[h["seq"]].lower()), None)
        if hit is None:
            missing.append(v)
        else:
            found.append(v)
            if where is None:
                where = hit
    if where is None or missing:
        return {"answer": None, "verified": False, "raw": reply[:200], "unverified": missing[:5], "hits": ids}
    txt = texts[where["seq"]]
    quote = next((ln.strip() for ln in txt.split("\n") if found[0].lower() in ln.lower()), found[0])
    return {"answer": re.sub(r"\s*\[#\d+\]", "", reply).strip(), "seq": where["seq"], "session": where["session"], "quote": quote[:200],
            "verified": True, "unverified": missing[:5], "raw": None, "hits": ids}


NOTE_SYS = ("The user's new request to a coding agent is below, with chunks from the session that may bear on it, each headed [#SEQ session kind]. "
            "List the facts from the chunks that the agent will need for this request: exact values, paths, commands already run and what they returned, errors, decisions. "
            "One fact per line, as: - <fact, quoting the exact value> [#SEQ]. At most 6 lines. If nothing in the chunks bears on the request, reply exactly: nothing relevant.")

def note(request, session, k=8):
    hits = search(request, session, k, EVIDENCE)["hits"]
    if not hits:
        return {"facts": []}
    texts = {h["seq"]: chunk_text(h["session"], h["seq"]) for h in hits}
    body = "".join(f"\n[#{h['seq']} {h['session']} {h['kind']}]\n{texts[h['seq']][:CHUNK_READ]}\n" for h in hits)
    reply = chat(NOTE_SYS, body + "\n\nRequest: " + request, 300)
    facts = []
    if "nothing relevant" in reply.lower()[:60]:
        return {"facts": []}
    for line in reply.split("\n"):
        line = line.strip().lstrip("-*• ").strip()
        if not line:
            continue
        named = [int(x) for x in re.findall(r"\[#(\d+)\]", line)]
        text = re.sub(r"\s*\[#\d+\]", "", line).strip()
        vals = claims(text, request)
        if not vals:
            continue
        order = sorted(hits, key=lambda h: 0 if h["seq"] in named else 1)
        where = None; ok = 0
        for v in vals:
            h = next((h for h in order if v.lower() in texts[h["seq"]].lower()), None)
            if h is not None:
                ok += 1
                if where is None:
                    where = h; first = v
        # every asserted value must be in some hit chunk, else the line is dropped
        if where is None or ok < len(vals):
            continue
        quote = next((ln.strip() for ln in texts[where["seq"]].split("\n") if first.lower() in ln.lower()), first)
        facts.append({"seq": where["seq"], "session": where["session"], "quote": quote[:200], "fact": text[:300]})
    return {"facts": facts[:6]}


# ---------------- ledger ----------------
LEDGER_SYS = ("Below are the chunks of one turn of a coding-agent session, each headed [#SEQ kind]. Extract what a later session would need to know: "
              "decisions taken, facts established (values, paths, versions), approaches that failed and why, files changed. "
              "One per line, as: - <decision|fact|failure|file>: <one sentence quoting the exact value> [#SEQ]. At most 8 lines. If the turn established nothing, reply exactly: nothing.")

def consolidate(session, from_seq, to_seq):
    rows = db.execute("select seq, kind, text from chunks where session=? and seq between ? and ? and kind!='ledger' order by seq", (session, from_seq, to_seq)).fetchall()
    if not rows:
        return {"records": 0}
    body = ""; budget = 24_000
    for seq, kind, text in rows:
        piece = f"\n[#{seq} {kind}]\n{text[:2000]}\n"
        if len(body) + len(piece) > budget:
            break
        body += piece
    reply = chat(LEDGER_SYS, body, 400)
    n = 0
    texts = {seq: text for seq, kind, text in rows}
    if reply.strip().lower().startswith("nothing"):
        return {"records": 0}
    for line in reply.split("\n"):
        line = line.strip().lstrip("-*• ").strip()
        if not line:
            continue
        kind, _, rest = line.partition(":")
        kind = kind.strip().lower()
        if kind not in ("decision", "fact", "failure", "file"):
            kind, rest = "fact", line
        named = [int(x) for x in re.findall(r"\[#(\d+)\]", rest)]
        fact = re.sub(r"\s*\[#\d+\]", "", rest).strip()
        vals = claims(fact, "")
        where = None; ok = 0
        for v in vals:
            sq = next((s for s in ([x for x in named if x in texts] + list(texts)) if v.lower() in texts[s].lower()), None)
            if sq is not None:
                ok += 1
                if where is None:
                    where = sq; first = v
        if not fact or where is None or ok < len(vals):
            continue
        quote = next((ln.strip() for ln in texts[where].split("\n") if first.lower() in ln.lower()), first)
        db.execute("insert into ledger values (?,?,?,?,?,?)", (session, where, kind, fact, quote[:200], time.time()))
        lseq = 1_000_000 + db.execute("select count(*) from ledger").fetchone()[0]
        text = f"{kind}: {fact}\nfrom [#{where}]: {quote[:200]}"
        cur = db.execute("insert into chunks values (?,?,?,?,?,?)", (session, lseq, "ledger", text, f"{kind}: {fact[:100]}", time.time()))
        db.execute("insert into chunks_fts(rowid, text, line) values (?,?,?)", (cur.lastrowid, text, fact))
        v = embed(text); db.execute("insert or replace into vec values (?,?,?)", (session, lseq, v.tobytes())); vecs[(session, lseq)] = v
        n += 1
    db.commit()
    return {"records": n}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, code, obj):
        body = json.dumps(obj).encode()
        try:
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except BrokenPipeError:
            pass

    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/status":
            n = db.execute("select count(*) from chunks where kind!='ledger'").fetchone()[0]
            l = db.execute("select count(*) from ledger").fetchone()[0]
            self.reply(200, {"reader": READER, "embed": EMBED, "chunks": n, "ledger": l, "db": DB})
        elif u.path == "/lines":
            s = parse_qs(u.query).get("session", [""])[0]
            self.reply(200, lines(s))
        else:
            self.reply(404, {"error": "no such route"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        try:
            req = json.loads(self.rfile.read(n) or b"{}")
        except json.JSONDecodeError:
            return self.reply(400, {"error": "bad json"})
        try:
            if self.path == "/index":
                out = index(req["session"], int(req["seq"]), req.get("kind", "tool output"), req.get("text", ""))
            elif self.path == "/search":
                out = search(req.get("query", ""), req.get("session"), int(req.get("k", 8)), req.get("kinds"))
            elif self.path == "/recall":
                out = recall(req.get("question", ""), req.get("session"), int(req.get("k", 8)))
            elif self.path == "/note":
                out = note(req.get("request", ""), req.get("session"), int(req.get("k", 6)))
            elif self.path == "/consolidate":
                out = consolidate(req["session"], int(req.get("from_seq", 0)), int(req.get("to_seq", 1 << 40)))
            else:
                return self.reply(404, {"error": "no such route"})
        except Exception as e:  # noqa: BLE001
            print(f"memoryd: {self.path}: {type(e).__name__}: {e}", flush=True)
            return self.reply(500, {"error": f"{type(e).__name__}: {e}"})
        self.reply(200, out)


if __name__ == "__main__":
    boot()
    srv = HTTPServer(("127.0.0.1", PORT), H)
    print(f"memoryd: listening on 127.0.0.1:{PORT}", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        sys.exit(0)
