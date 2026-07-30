"""The context type the pipeline should be threading.

Defined, documented, and not yet used by anything.
"""

from dataclasses import dataclass
from typing import Optional


@dataclass(frozen=True)
class Ctx:
    """One request as it moves through the chain.

    Frozen on purpose: stages used to mutate the shared dict and the order they ran
    in silently decided the result. A stage that needs a change returns a new Ctx
    via `dataclasses.replace`.
    """

    path: str
    method: str
    user: Optional[str] = None
    trace: Optional[str] = None
    status: int = 200
    body: str = ""
