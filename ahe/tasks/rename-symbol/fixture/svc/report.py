from .paths import PathResolver

TEMPLATE = "resolve({ref}) -> {path}"


def render(root, refs):
    r = PathResolver(root)
    lines = []
    for ref in refs:
        lines.append(TEMPLATE.format(ref=ref, path=r.resolve(ref)))
    return "\n".join(lines)


def resolve(ref):
    """A module-level helper that happens to share the name. Not a method."""
    return ref.strip()
