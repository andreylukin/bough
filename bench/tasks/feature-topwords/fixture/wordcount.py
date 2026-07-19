#!/usr/bin/env python3
"""wordcount: print line and word counts for a text file.

Usage: python3 wordcount.py FILE
"""
import sys


def counts(text):
    return len(text.splitlines()), len(text.split())


def main(argv):
    if len(argv) < 1:
        print("usage: wordcount.py FILE", file=sys.stderr)
        return 2
    with open(argv[0], encoding="utf-8") as f:
        lines, words = counts(f.read())
    print(f"{lines} {words}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
