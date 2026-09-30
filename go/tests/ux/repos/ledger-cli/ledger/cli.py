import sys

from .core import balance, by_category


def main(argv=None):
    argv = argv or sys.argv[1:]
    lines = open(argv[1]).read().splitlines()
    if argv[0] == "balance":
        print(balance(lines))
    elif argv[0] == "report":
        for cat, amt in sorted(by_category(lines).items()):
            print(f"{cat:<16}{amt:>10}")


if __name__ == "__main__":
    main()
