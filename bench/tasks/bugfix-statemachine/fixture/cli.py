"""CLI: python3 cli.py EVENT [EVENT ...] -- print the state after each event."""

import sys

from dispatcher import dispatch
from machine import TRANSITIONS, new_ticket


def main(events):
    ctx = new_ticket()
    for event in events:
        print(dispatch(TRANSITIONS, ctx, event))


if __name__ == "__main__":
    main(sys.argv[1:])
