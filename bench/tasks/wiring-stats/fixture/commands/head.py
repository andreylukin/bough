def run(args):
    """Print the first three lines of the file."""
    with open(args[0], encoding="utf-8") as f:
        for i, line in enumerate(f):
            if i == 3:
                break
            print(line, end="")
    return 0
