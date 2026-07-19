"""CLI: python3 dump.py FILE -- print each record on one line, values repr'd."""

import sys

from recio.reader import read_records


def main(path):
    for rec in read_records(path):
        print(" ".join("{}={!r}".format(key, rec[key]) for key in ("id", "name", "qty", "note")))


if __name__ == "__main__":
    main(sys.argv[1])
