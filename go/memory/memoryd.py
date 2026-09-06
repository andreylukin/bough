"""memoryd: bough's local memory model.

A hybrid (Mamba-2 + attention) model on mlx-lm holds one recurrent state
per session. Everything the agent sees is appended to that state, in
order, once. Questions fork the state, generate an answer, and drop the
fork, so the memory itself is never polluted by its own answers. The
state is saved to disk with save_prompt_cache and reloaded on demand,
which is what makes it survive a restart without re-reading history.

HTTP, JSON bodies, single-threaded on purpose: MLX streams are
thread-local, so every array must be touched from the one thread that
created it. Requests queue; the state is single anyway.
  POST /ingest {session, text, seq?}      append text; seq = history seq (dedupes on resume)
  POST /ask    {session, question, max_tokens?}  answer from the state
  POST /save   {session}                  write the state to disk
  POST /load   {session}                  read it back (no-op if resident)
  GET  /status                            sessions, token counts, model
"""

import copy
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

import mlx.core as mx
from mlx_lm import load
from mlx_lm.generate import generate_step
from mlx_lm.models.cache import load_prompt_cache, make_prompt_cache, save_prompt_cache

MODEL = os.environ.get("BOUGH_MEMORY_MODEL", "mlx-community/granite-4.0-h-small-4bit")
STATE_DIR = os.path.expanduser(os.environ.get("BOUGH_MEMORY_STATE", "~/.bough/memory/state"))
PORT = int(os.environ.get("BOUGH_MEMORY_PORT", "8765"))
PREFILL = 2048

SYSTEM = (
    "You are the memory of a coding-agent session. The user message is the "
    "session as it happened: the user's requests, the agent's replies, and "
    "every tool output in full, each headed [#SEQ kind]. When a Question "
    "arrives, answer it from the session only, precisely, quoting values, "
    "paths and numbers verbatim. If the session does not contain the "
    "answer, say exactly: not in memory."
)

model = tok = None
prefix_ids = suffix_text = None
sessions = {}  # name -> {"cache": [...], "tokens": int, "chars": int}


def boot():
    global model, tok, prefix_ids, suffix_text
    t = time.time()
    model, tok = load(MODEL)
    marker = "@@SESSION@@"
    rendered = tok.apply_chat_template(
        [{"role": "system", "content": SYSTEM}, {"role": "user", "content": marker}],
        tokenize=False, add_generation_prompt=True)
    pre, suf = rendered.split(marker)
    prefix_ids = tok.encode(pre, add_special_tokens=False)
    suffix_text = suf
    print(f"memoryd: {MODEL} loaded in {time.time()-t:.0f}s; prefix {len(prefix_ids)} tokens", flush=True)


def feed(cache, ids):
    """Prefill ids into cache in chunks; no logits kept."""
    for i in range(0, len(ids), PREFILL):
        chunk = mx.array(ids[i:i + PREFILL])[None]
        model(chunk, cache=cache)
        mx.eval([c.state for c in cache])
    mx.clear_cache()


def session(name):
    s = sessions.get(name)
    if s is None:
        cache = make_prompt_cache(model)
        feed(cache, prefix_ids)
        s = sessions[name] = {"cache": cache, "tokens": len(prefix_ids), "chars": 0, "seq": 0}
    return s


def ingest(name, text, seq=0):
    """Append text; seq is the history seq it came from (the high-water
    mark a resumed session continues from). Already-seen seqs are skipped."""
    s = session(name)
    if seq and seq <= s["seq"]:
        return {"tokens": s["tokens"], "added": 0, "skipped": True, "seq": s["seq"]}
    ids = tok.encode(text, add_special_tokens=False)
    t = time.time()
    feed(s["cache"], ids)
    s["tokens"] += len(ids)
    s["chars"] += len(text)
    s["seq"] = max(s["seq"], seq)
    return {"tokens": s["tokens"], "added": len(ids), "seq": s["seq"], "secs": round(time.time() - t, 2)}


def ask(name, question, max_tokens=200):
    s = session(name)
    fork = copy.deepcopy(s["cache"])
    q = "\n\nQuestion: " + question.strip() + suffix_text
    ids = mx.array(tok.encode(q, add_special_tokens=False))
    out = []
    t = time.time()
    for token, _ in generate_step(ids, model, prompt_cache=fork, max_tokens=max_tokens):
        if token in tok.eos_token_ids:
            break
        out.append(token)
    del fork
    mx.clear_cache()
    return {"answer": tok.decode(out).strip(), "tokens": s["tokens"], "secs": round(time.time() - t, 2)}


def state_path(name):
    safe = "".join(c if c.isalnum() or c in "-_." else "_" for c in name)
    return os.path.join(STATE_DIR, safe + ".safetensors")


def save(name):
    s = sessions.get(name)
    if s is None:
        return {"saved": False}
    os.makedirs(STATE_DIR, exist_ok=True)
    save_prompt_cache(state_path(name), s["cache"], {"tokens": str(s["tokens"]), "chars": str(s["chars"]), "seq": str(s["seq"])})
    return {"saved": True, "path": state_path(name), "tokens": s["tokens"], "seq": s["seq"]}


def load_state(name):
    if name in sessions:
        return {"loaded": True, "resident": True, "tokens": sessions[name]["tokens"], "seq": sessions[name]["seq"]}
    p = state_path(name)
    if not os.path.exists(p):
        return {"loaded": False}
    cache, meta = load_prompt_cache(p, return_metadata=True)
    sessions[name] = {"cache": cache, "tokens": int(meta.get("tokens", 0)), "chars": int(meta.get("chars", 0)), "seq": int(meta.get("seq", 0))}
    return {"loaded": True, "resident": False, "tokens": sessions[name]["tokens"], "seq": sessions[name]["seq"]}


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
            pass  # the client gave up waiting; the work is done regardless

    def do_GET(self):
        if self.path == "/status":
            self.reply(200, {"model": MODEL, "sessions": {k: {"tokens": v["tokens"], "chars": v["chars"], "seq": v["seq"]} for k, v in sessions.items()}})
        else:
            self.reply(404, {"error": "no such route"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        try:
            req = json.loads(self.rfile.read(n) or b"{}")
        except json.JSONDecodeError:
            return self.reply(400, {"error": "bad json"})
        name = req.get("session", "default")
        try:
            if self.path == "/ingest":
                out = ingest(name, req.get("text", ""), int(req.get("seq", 0)))
            elif self.path == "/ask":
                out = ask(name, req.get("question", ""), int(req.get("max_tokens", 200)))
            elif self.path == "/save":
                out = save(name)
            elif self.path == "/load":
                out = load_state(name)
            else:
                return self.reply(404, {"error": "no such route"})
        except Exception as e:  # noqa: BLE001 - one bad request must not kill the daemon
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
