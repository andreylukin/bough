from settings import resolve
from textutil import wrap


def run(args):
    """Print how many output lines `render` would produce."""
    cfg = resolve()
    total = 0
    with open(args[0], encoding="utf-8") as f:
        for raw in f:
            total += len(wrap(raw.rstrip("\n"), cfg["width"]))
    print(total)
    return 0
