"""A DNS-ish stub. Its `resolve` is a DIFFERENT method on a different class."""


class NameResolver:
    def __init__(self, table):
        self.table = table

    def resolve(self, host):
        """Resolve a hostname to an address. Unrelated to PathResolver.resolve."""
        return self.table.get(host, "0.0.0.0")
