#!/usr/bin/env python3
"""A tiny task tracker backed by tasks.json in the current directory.

Usage:
  tasks.py add <text>     append a task, print "#<id>"
  tasks.py list           print each task, one per line
"""
import json
import os
import sys

STORE = "tasks.json"


def load():
    if not os.path.exists(STORE):
        return []
    with open(STORE) as fh:
        return json.load(fh)


def save(tasks):
    with open(STORE, "w") as fh:
        json.dump(tasks, fh)


def render(t):
    box = "[x]" if t["done"] else "[ ]"
    return f"#{t['id']} {box} {t['text']}"


def cmd_add(args):
    text = args[0]
    tasks = load()
    tid = max((t["id"] for t in tasks), default=0) + 1
    tasks.append({"id": tid, "text": text, "done": False})
    save(tasks)
    print(f"#{tid}")


def cmd_list(args):
    for t in load():
        print(render(t))


def main(argv):
    if not argv:
        print("usage: tasks.py <add|list> ...", file=sys.stderr)
        return 2
    cmd, rest = argv[0], argv[1:]
    if cmd == "add":
        return cmd_add(rest)
    if cmd == "list":
        return cmd_list(rest)
    print(f"unknown command: {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]) or 0)
