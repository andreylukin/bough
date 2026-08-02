"""The published surface. PROTECTED: do not modify."""

from .core import Core, PluginError

__all__ = ["Registry", "PluginError"]


class Registry:
    """A plugin registry. Two registries are independent of each other."""

    def __init__(self, options=None):
        self._core = Core(options)

    def register(self, name, fn, tags=None):
        self._core.register(name, fn, tags)

    def get(self, name):
        return self._core.get(name)

    def names(self):
        return self._core.names()

    def by_tag(self, tag):
        return self._core.by_tag(tag)

    def options(self):
        return self._core.options
