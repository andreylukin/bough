"""The target type. Already written — migrate onto it, do not change it."""

from dataclasses import dataclass


@dataclass(frozen=True)
class Result:
    ok: bool
    value: object = None
    error: str = None

    def unwrap(self):
        if not self.ok:
            raise ValueError(self.error)
        return self.value


def Ok(value=None):
    return Result(True, value=value)


def Err(error):
    return Result(False, error=error)
