"""The published surface. PROTECTED: do not modify."""

from .steps import Migrator, MigrationError

__all__ = ["migrate", "MigrationError"]

LATEST = 4


def migrate(record: dict, to: int = LATEST) -> dict:
    """Bring `record` to schema version `to`. Never mutates the input."""
    return Migrator().run(record, to)
