from .dns import NameResolver
from .paths import PathResolver

HELP = "resolve: map a ref to a path"


class Index:
    def __init__(self, root, hosts):
        self.paths = PathResolver(root)
        self.names = NameResolver(hosts)

    def entry(self, ref, host):
        return {
            "path": self.paths.resolve_path(ref),
            "addr": self.names.resolve(host),
            "help": HELP,
        }

    def batch(self, refs):
        return [self.paths.resolve_path(r) for r in refs]
