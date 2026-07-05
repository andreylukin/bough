#!/usr/bin/env python3
"""Read bough conversation history straight from the SQLite store.

bough keeps everything in ~/.bough/bough.db (override with BOUGH_DB):
  sessions  — id, parent_id, title, kind (root|fork|worker|compaction|subagent),
              workspace, origin_id/origin_message_id (lineage for forks/compactions)
  messages  — role (user|supervisor|worker|system), parts (JSON Part[]), pending
  turns     — per-turn status (running|done|error|orphaned) + last checkpoint
  net_events— gated network requests (host, action, verdict, reason)

Messages within a session are linear; branching happens by creating a child
session (fork/subagent/compaction) that points at its parent. No server needed —
this reads the DB read-only.
"""
import argparse
import datetime as dt
import json
import os
import sqlite3
import sys

DB_PATH = os.path.expanduser(os.environ.get("BOUGH_DB", "~/.bough/bough.db"))


def connect():
    if not os.path.exists(DB_PATH):
        sys.exit(f"no bough db at {DB_PATH} (set BOUGH_DB to override)")
    db = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
    db.row_factory = sqlite3.Row
    return db


def fmt_ts(ms):
    if not ms:
        return "?"
    return dt.datetime.fromtimestamp(ms / 1000).strftime("%Y-%m-%d %H:%M")


def truncate(s, n):
    s = s or ""
    if n and len(s) > n:
        return s[:n] + f"\n      … [+{len(s) - n} chars, --full for all]"
    return s


def indent(s):
    return "  " + s.replace("\n", "\n  ")


# ---- list -------------------------------------------------------------------

def cmd_list(args):
    db = connect()
    rows = db.execute(
        """
        SELECT s.id, s.title, s.kind, s.workspace, s.created_at, s.archived_at,
               (SELECT count(*) FROM messages m WHERE m.session_id = s.id AND m.role = 'user') AS turns,
               (SELECT max(m.created_at) FROM messages m WHERE m.session_id = s.id) AS updated
        FROM sessions s
        """
    ).fetchall()
    out = []
    for r in rows:
        if args.no_empty and r["turns"] == 0:
            continue
        if not args.archived and r["archived_at"]:
            continue
        ws = r["workspace"] or ""
        if args.project and args.project not in ws:
            continue
        out.append({
            "id": r["id"],
            "title": r["title"],
            "kind": r["kind"],
            "workspace": ws,
            "turns": r["turns"],
            "updated": r["updated"] or r["created_at"],
        })
    out.sort(key=lambda r: r["updated"], reverse=True)
    if args.limit:
        out = out[: args.limit]
    if args.json:
        print(json.dumps(out, indent=2))
        return
    for r in out:
        kind = "" if r["kind"] == "root" else f"  [{r['kind']}]"
        print(f"{r['id']}  {fmt_ts(r['updated'])}  {r['turns']:>3}t  {os.path.basename(r['workspace'])}{kind}")
        print(f"    {r['title'][:70]}")


# ---- show -------------------------------------------------------------------

def call_digest(name, inp):
    """One-line-ish summary of a tool call's input."""
    if not isinstance(inp, dict):
        return json.dumps(inp)
    if name == "bash":
        return inp.get("command", "")
    if name in ("read_file", "write_file", "edit_file"):
        return inp.get("path", json.dumps(inp))
    if name == "run_steps":
        code = inp.get("code", "")
        check = inp.get("check")
        done = inp.get("done")
        tail = []
        if check:
            tail.append(f"[check] {check}")
        if done:
            tail.append("[done]")
        return code + ("\n" + "\n".join(tail) if tail else "")
    return json.dumps(inp)


def render_part(p, maxlen, quiet):
    ty = p.get("type")
    if ty == "text":
        return indent(truncate(p.get("text", ""), maxlen))
    if ty == "reasoning":
        return None if quiet else "  ~ thinking: " + truncate(p.get("text", ""), maxlen)
    if ty == "tool_call":
        return f"  ▶ {p.get('name')}: " + truncate(call_digest(p.get("name"), p.get("input")), maxlen)
    if ty == "tool_result":
        out = p.get("output")
        if not isinstance(out, str):
            out = json.dumps(out)
        mark = "✗" if p.get("isError") else "←"
        return f"  {mark} " + truncate(out, maxlen)
    return indent(truncate(json.dumps(p), maxlen))


ROLE_LABEL = {"user": "USER", "supervisor": "ASSISTANT", "worker": "WORKER", "system": "SYSTEM"}


def cmd_show(args):
    db = connect()
    s = db.execute("SELECT * FROM sessions WHERE id = ?", (args.session,)).fetchone()
    if not s:
        # allow unambiguous id prefixes
        matches = db.execute("SELECT * FROM sessions WHERE id LIKE ?", (args.session + "%",)).fetchall()
        if len(matches) == 1:
            s = matches[0]
        elif matches:
            sys.exit("ambiguous prefix: " + ", ".join(m["id"] for m in matches))
        else:
            sys.exit(f"no session {args.session} in {DB_PATH}")
    maxlen = 0 if args.full else args.maxlen

    print(f"# session {s['id']}  ({s['workspace'] or 'no workspace'})")
    line = f"# {s['title']!r}  kind={s['kind']}  created {fmt_ts(s['created_at'])}"
    if s["parent_id"]:
        line += f"  parent={s['parent_id']}"
    if s["origin_id"]:
        line += f"  origin={s['origin_id']}"
    print(line)
    children = db.execute(
        "SELECT id, kind, title FROM sessions WHERE parent_id = ? OR origin_id = ?",
        (s["id"], s["id"]),
    ).fetchall()
    for c in children:
        print(f"# child: {c['id']}  [{c['kind']}]  {c['title'][:50]}")

    msgs = db.execute(
        "SELECT * FROM messages WHERE session_id = ? ORDER BY created_at, rowid", (s["id"],)
    ).fetchall()
    for m in msgs:
        role = ROLE_LABEL.get(m["role"], m["role"].upper())
        if m["role"] == "system" and args.quiet:
            continue
        pending = ""
        if m["pending"]:
            t = db.execute(
                "SELECT status, step FROM turns WHERE message_id = ?", (m["id"],)
            ).fetchone()
            pending = f"  (pending: {t['status']} @ {t['step']})" if t else "  (pending)"
        print(f"\n{role}:{pending}")
        for p in json.loads(m["parts"]):
            line = render_part(p, maxlen, args.quiet)
            if line is not None:
                print(line)

    if args.net:
        net = db.execute(
            "SELECT ts, host, verb, action, verdict, reason FROM net_events "
            "WHERE session_id = ? ORDER BY ts", (s["id"],)
        ).fetchall()
        if net:
            print("\n# net events")
            for n in net:
                print(f"  {fmt_ts(n['ts'])}  {n['verdict']:<7}  {n['host']}  {n['action']}  ({n['reason']})")


def main():
    p = argparse.ArgumentParser(description="Read bough conversation history from bough.db.")
    sub = p.add_subparsers(dest="cmd", required=True)

    pl = sub.add_parser("list", help="list sessions, newest first")
    pl.add_argument("-p", "--project", help="filter by substring of workspace path")
    pl.add_argument("-n", "--limit", type=int, default=30)
    pl.add_argument("--archived", action="store_true", help="include archived sessions")
    pl.add_argument("--no-empty", action="store_true", help="hide sessions with zero turns")
    pl.add_argument("--json", action="store_true")
    pl.set_defaults(func=cmd_list)

    ps = sub.add_parser("show", help="dump a session transcript")
    ps.add_argument("session", help="session id (unambiguous prefix ok)")
    ps.add_argument("--full", action="store_true", help="don't truncate long output")
    ps.add_argument("--maxlen", type=int, default=600, help="truncation length (default 600)")
    ps.add_argument("-q", "--quiet", action="store_true", help="hide system messages and reasoning")
    ps.add_argument("--net", action="store_true", help="append the session's gated network requests")
    ps.set_defaults(func=cmd_show)

    args = p.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
