#!/usr/bin/env python3
"""linefmt: tiny line formatter. Usage: python3 cli.py COMMAND [FILE]"""
import sys

from commands.count import run as count_run
from commands.render import run as render_run

COMMANDS = {
    "render": render_run,
    "count": count_run,
}


def main(argv):
    if not argv or argv[0] not in COMMANDS:
        print("usage: cli.py {" + ",".join(sorted(COMMANDS)) + "} [FILE]", file=sys.stderr)
        return 2
    return COMMANDS[argv[0]](argv[1:])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
