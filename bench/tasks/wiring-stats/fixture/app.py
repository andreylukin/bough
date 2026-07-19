#!/usr/bin/env python3
"""Mini file toolkit. Usage: python3 app.py COMMAND FILE"""
import sys

from commands.count import run as count_run
from commands.head import run as head_run

COMMANDS = {
    "count": count_run,
    "head": head_run,
}


def main(argv):
    if not argv or argv[0] not in COMMANDS:
        print("usage: app.py {" + ",".join(sorted(COMMANDS)) + "} FILE", file=sys.stderr)
        return 2
    return COMMANDS[argv[0]](argv[1:])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
