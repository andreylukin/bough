def run(args):
    """Print the number of lines in the file."""
    with open(args[0], encoding="utf-8") as f:
        print(sum(1 for _ in f))
    return 0
