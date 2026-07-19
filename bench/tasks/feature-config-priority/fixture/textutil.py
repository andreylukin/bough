"""Greedy word-wrapping, deterministic and dependency-free."""


def wrap(text, width):
    """Split text into lines of at most `width` chars, breaking at spaces."""
    words = text.split()
    if not words:
        return [""]
    lines = []
    cur = words[0]
    for word in words[1:]:
        if len(cur) + 1 + len(word) <= width:
            cur += " " + word
        else:
            lines.append(cur)
            cur = word
    lines.append(cur)
    return lines
