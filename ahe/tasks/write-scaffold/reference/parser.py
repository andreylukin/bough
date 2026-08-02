"""A small DSV parser."""


class ParseError(Exception):
    pass


def _split_records(text, delimiter):
    """Yield lists of raw (value, was_quoted) pairs, one list per physical record.

    Quoting is resolved here because a quoted field may contain newlines, so the
    record boundary is not simply a line break.
    """
    records = []
    field = []
    quoted = False
    row = []
    i = 0
    n = len(text)
    in_quotes = False
    while i < n:
        ch = text[i]
        if in_quotes:
            if ch == '"':
                if i + 1 < n and text[i + 1] == '"':
                    field.append('"')
                    i += 2
                    continue
                in_quotes = False
                i += 1
                # R2: nothing but a delimiter or a line end may follow.
                if i < n and text[i] not in (delimiter, "\n", "\r"):
                    raise ParseError("text after a closing quote")
                continue
            field.append(ch)
            i += 1
            continue
        if ch == '"' and not field:
            in_quotes = True
            quoted = True
            i += 1
            continue
        if ch == delimiter:
            row.append(("".join(field), quoted))
            field, quoted = [], False
            i += 1
            continue
        if ch in "\r\n":
            row.append(("".join(field), quoted))
            records.append(row)
            field, quoted, row = [], False, []
            if ch == "\r" and i + 1 < n and text[i + 1] == "\n":
                i += 2
            else:
                i += 1
            continue
        field.append(ch)
        i += 1
    if in_quotes:
        raise ParseError("unterminated quote")
    if field or quoted or row:
        row.append(("".join(field), quoted))
        records.append(row)
    return records


def _blank(row):
    # R3: one unquoted field that is whitespace only.
    return len(row) == 1 and not row[0][1] and row[0][0].strip() == ""


def _coerce(value, was_quoted):
    if was_quoted:
        return value  # R5: a quoted field is always a string.
    v = value.strip(" \t")  # R2
    if v == "":
        return None
    body = v[1:] if v.startswith("-") else v
    if body.isdigit() and body != "":
        return int(v)
    return v


def parse(text, delimiter=","):
    records = [r for r in _split_records(text, delimiter) if not _blank(r)]
    if not records:
        return []  # R7

    header = []
    for value, was_quoted in records[0]:
        name = value if was_quoted else value.strip(" \t")
        if name == "":
            raise ParseError("empty header name")  # R6
        if name in header:
            raise ParseError(f"duplicate header name: {name}")  # R6
        header.append(name)

    out = []
    for row in records[1:]:
        if len(row) > len(header):
            raise ParseError("record has more fields than the header")  # R4
        item = {}
        for i, name in enumerate(header):
            if i < len(row):
                item[name] = _coerce(*row[i])
            else:
                item[name] = None  # R4
        out.append(item)
    return out
