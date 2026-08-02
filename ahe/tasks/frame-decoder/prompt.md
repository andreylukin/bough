`wire/` frames a byte stream: a 4-byte big-endian length, then that many payload
bytes. Chunks arrive from a socket, so they split anywhere. The decoder does not
match the spec below. Fix it.

## Spec

1. **Zero-length frames are legal.** A header of `0` is a complete frame carrying
   `b""`, and `feed` emits it like any other. It is not end-of-stream and it is
   not something to skip.
2. **Any split is legal.** A chunk may end in the middle of a header, in the
   middle of a payload, or exactly on a boundary. Feeding a stream one byte at a
   time must produce exactly the same frames, in the same order, as feeding it
   whole.
3. **Oversized frames are rejected early.** If the declared length exceeds
   `max_frame`, `feed` raises `ProtocolError` **as soon as the header is
   readable** — it must not buffer the body first. A stream that announces a 4 GB
   frame must not cost 4 GB to reject.
4. **An error poisons the decoder.** Once `feed` has raised `ProtocolError`, the
   stream is unrecoverable: every later `feed` and `pending` raises
   `ProtocolError` too. There is no resynchronisation.
5. **`pending`** is the number of buffered bytes not yet emitted, counting a
   partial header.

Frames that completed *before* the bad header in the same chunk are lost with it —
`feed` raises rather than returning them.

## Constraints

- `wire/api.py` is the published surface and is **protected**: do not modify it.
- `test_wire.py` is the checked-in test suite. It must still pass.
