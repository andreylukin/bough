"""The published surface. PROTECTED: do not modify."""

from .codec import Codec, ProtocolError

__all__ = ["Decoder", "ProtocolError"]


class Decoder:
    """Frames a byte stream that arrives in arbitrary chunks."""

    def __init__(self, max_frame: int = 1 << 20):
        self._codec = Codec(max_frame)

    def feed(self, chunk: bytes) -> list:
        """Consume `chunk`; return every frame that completed because of it."""
        return self._codec.feed(chunk)

    def pending(self) -> int:
        """Bytes buffered but not yet part of a completed frame."""
        return self._codec.pending()
