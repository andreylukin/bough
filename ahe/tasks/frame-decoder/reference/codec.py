"""Length-prefixed framing: a 4-byte big-endian length, then that many bytes."""

HEADER = 4


class ProtocolError(Exception):
    pass


class Codec:
    def __init__(self, max_frame):
        self.max_frame = max_frame
        self.buf = b""
        self.poisoned = None

    def _check(self):
        # R4: once broken, always broken.
        if self.poisoned is not None:
            raise ProtocolError(self.poisoned)

    def _poison(self, why):
        self.poisoned = why
        self.buf = b""
        raise ProtocolError(why)

    def pending(self):
        self._check()
        return len(self.buf)

    def feed(self, chunk):
        self._check()
        self.buf += chunk
        out = []
        # R1 + R2: `>=`, so a header that is exactly buffered is read, and a
        # zero-length frame completes on the header alone.
        while len(self.buf) >= HEADER:
            size = int.from_bytes(self.buf[:HEADER], "big")
            # R3: reject on the header, before the body is buffered.
            if size > self.max_frame:
                self._poison(f"frame of {size} exceeds max_frame {self.max_frame}")
            if len(self.buf) < HEADER + size:
                break
            out.append(self.buf[HEADER:HEADER + size])
            self.buf = self.buf[HEADER + size:]
        return out
