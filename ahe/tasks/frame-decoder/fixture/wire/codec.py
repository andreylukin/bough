"""Length-prefixed framing: a 4-byte big-endian length, then that many bytes."""

HEADER = 4


class ProtocolError(Exception):
    pass


class Codec:
    def __init__(self, max_frame):
        self.max_frame = max_frame
        self.buf = b""

    def pending(self):
        return len(self.buf)

    def feed(self, chunk):
        self.buf += chunk
        out = []
        while len(self.buf) > HEADER:
            size = int.from_bytes(self.buf[:HEADER], "big")
            if len(self.buf) < HEADER + size:
                break
            out.append(self.buf[HEADER:HEADER + size])
            self.buf = self.buf[HEADER + size:]
        return out
