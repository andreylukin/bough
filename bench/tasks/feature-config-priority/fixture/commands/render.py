from settings import resolve
from textutil import wrap


def run(args):
    """Wrap each input line to the configured width, prefixing output lines."""
    cfg = resolve()
    with open(args[0], encoding="utf-8") as f:
        for raw in f:
            for out in wrap(raw.rstrip("\n"), cfg["width"]):
                print(cfg["prefix"] + out)
    return 0
