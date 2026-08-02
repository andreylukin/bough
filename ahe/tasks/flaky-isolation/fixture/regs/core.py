"""Registry internals."""

_PLUGINS = {}
_TAGS = {}
_BY_TAG_CACHE = {}


class PluginError(Exception):
    pass


class Core:
    def __init__(self, options=None, defaults={}):
        self.options = defaults
        if options:
            self.options.update(options)

    def register(self, name, fn, tags=None):
        if name in _PLUGINS:
            raise PluginError(f"duplicate plugin {name}")
        _PLUGINS[name] = fn
        _TAGS[name] = tuple(tags or ())
        _BY_TAG_CACHE.clear()

    def get(self, name):
        if name not in _PLUGINS:
            raise PluginError(f"unknown plugin {name}")
        return _PLUGINS[name]

    def names(self):
        return sorted(_PLUGINS)

    def by_tag(self, tag):
        if tag not in _BY_TAG_CACHE:
            _BY_TAG_CACHE[tag] = sorted(n for n in _PLUGINS if tag in _TAGS[n])
        return _BY_TAG_CACHE[tag]
