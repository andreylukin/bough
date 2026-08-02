"""Path handling."""


class PathResolver:
    """Turns a reference into an absolute path."""

    def __init__(self, root):
        self.root = root

    def resolve(self, ref):
        """Resolve `ref` against the root."""
        if ref.startswith("/"):
            return ref
        return f"{self.root}/{ref}"

    def resolve_all(self, refs):
        return [self.resolve(r) for r in refs]
