def flatten(value):
    """Depth-first flatten of arbitrarily nested lists/tuples.

    Strings are leaves, never iterated.
    """
    out = []
    for item in value:
        if isinstance(item, (list, tuple)):
            out.extend(flatten(item))
        else:
            out.append(item)
    return out
