"""Registry internals. Every piece of state belongs to one Core."""


class PluginError(Exception):
    pass


class Core:
    def __init__(self, options=None):
        # R3: a fresh dict per instance — never a shared default argument.
        self.options = dict(options or {})
        # R1: per-instance state, so two registries cannot see each other.
        self._plugins = {}
        self._tags = {}
        self._by_tag_cache = {}

    def register(self, name, fn, tags=None):
        if name in self._plugins:  # R2
            raise PluginError(f"duplicate plugin {name}")
        self._plugins[name] = fn
        self._tags[name] = tuple(tags or ())
        self._by_tag_cache.clear()  # R4

    def get(self, name):
        if name not in self._plugins:
            raise PluginError(f"unknown plugin {name}")
        return self._plugins[name]

    def names(self):
        return sorted(self._plugins)

    def by_tag(self, tag):
        if tag not in self._by_tag_cache:
            self._by_tag_cache[tag] = sorted(
                n for n in self._plugins if tag in self._tags[n]
            )
        return self._by_tag_cache[tag]
