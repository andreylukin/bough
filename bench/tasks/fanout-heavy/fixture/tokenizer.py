"""Split a command-ish line into tokens on whitespace, but keep double-quoted
spans together (quotes removed). A line that does not end in whitespace still
has its final token — nothing may be dropped.

    split('a "b c" d')   -> ['a', 'b c', 'd']
    split('one two')     -> ['one', 'two']
"""


def split(line):
    tokens = []
    buf = []
    in_quote = False
    for ch in line:
        if ch == '"':
            in_quote = not in_quote
        elif ch.isspace() and not in_quote:
            if buf:
                tokens.append("".join(buf))
                buf = []
        else:
            buf.append(ch)
    return tokens


def token_count(line):
    return len(split(line))
