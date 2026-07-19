"""Formatting settings. For now only compiled-in defaults."""

DEFAULTS = {
    "width": 60,
    "prefix": "",
}


def resolve():
    """Return the effective settings."""
    return dict(DEFAULTS)
