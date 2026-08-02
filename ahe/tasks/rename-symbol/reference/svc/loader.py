from .paths import PathResolver


class Loader:
    def __init__(self, root, files):
        self.paths = PathResolver(root)
        self.files = files

    def load(self, ref):
        # The docs still say "call resolve first" — that comment is prose, not code.
        return self.files.get(self.paths.resolve_path(ref))

    def load_many(self, refs):
        return [self.load(r) for r in refs]

    def describe(self):
        return "loader: uses PathResolver.resolve to map refs onto disk"
