"""Length-prefixed JSON frame codec for the Rust ↔ Python sidecar pipe.

WP-W2-04 §"Stdio framing":

    +---------+-------------------+
    | 4 bytes | UTF-8 JSON body   |
    | u32 BE  | (length bytes)    |
    +---------+-------------------+

Why length-prefixing instead of newline-delimited JSON? On Windows,
Python's stdout defaults to text-mode line buffering, which mangles
embedded newlines inside payloads (logs, prompts). Length prefixing is
binary-safe, stays predictable when one side is slow, and matches the
shape the Rust supervisor reads with `read_exact(4)` then
`read_exact(len)`.

The codec is intentionally tiny: two functions (`write_frame`,
`read_frame`) operating on raw bytes. Higher-level encode/decode is in
the caller — `__main__.py` does `json.dumps(...).encode("utf-8")`
before `write_frame`, and `json.loads(read_frame(...).decode("utf-8"))`
after.
"""

from __future__ import annotations

import struct
from typing import BinaryIO


# 4-byte big-endian unsigned length prefix. `!` selects network byte
# order which is canonical big-endian on every platform Python runs.
_LEN_FMT = "!I"
_LEN_SIZE = 4

# Hard cap on a single frame body. 16 MiB is far more than any realistic
# span/payload we emit — guards against a desync that would otherwise
# allocate gigabytes from a corrupted length prefix.
MAX_FRAME_BYTES = 16 * 1024 * 1024


class FrameError(Exception):
    """Raised when the frame stream is malformed or the peer hung up."""


def write_frame(stream: BinaryIO, body: bytes) -> None:
    """Write a single length-prefixed frame, then flush.

    `stream` must be opened in **binary** mode (`sys.stdout.buffer`,
    not `sys.stdout`). The flush is mandatory: the Rust side blocks on
    `read_exact(4)` and a missing flush deadlocks both processes.
    """
    if len(body) > MAX_FRAME_BYTES:
        raise FrameError(
            f"frame body too large: {len(body)} > {MAX_FRAME_BYTES}"
        )
    stream.write(struct.pack(_LEN_FMT, len(body)))
    stream.write(body)
    stream.flush()


def read_frame(stream: BinaryIO) -> bytes:
    """Read one length-prefixed frame from `stream`, return the body.

    Returns the raw body bytes (caller decodes JSON). Raises
    `FrameError` if the peer closed mid-frame or the length is
    impossible. End-of-stream **before any byte was read** is signalled
    by an empty bytes return — callers can treat that as a clean
    shutdown.
    """
    header = _read_exactly(stream, _LEN_SIZE, allow_eof=True)
    if header == b"":
        # Clean end-of-stream — peer hung up between frames.
        return b""
    if len(header) < _LEN_SIZE:
        raise FrameError(
            f"truncated length prefix: got {len(header)} of {_LEN_SIZE} bytes"
        )
    (length,) = struct.unpack(_LEN_FMT, header)
    if length > MAX_FRAME_BYTES:
        raise FrameError(
            f"frame body too large: {length} > {MAX_FRAME_BYTES}"
        )
    body = _read_exactly(stream, length, allow_eof=False)
    if len(body) < length:
        raise FrameError(
            f"truncated frame body: got {len(body)} of {length} bytes"
        )
    return body


def _read_exactly(stream: BinaryIO, n: int, *, allow_eof: bool) -> bytes:
    """Read exactly `n` bytes, returning `b""` on clean EOF if allowed.

    Python's `BinaryIO.read(n)` is allowed to return fewer than `n`
    bytes even when more data is coming; loop until we have the full
    buffer or a real EOF.
    """
    if n == 0:
        return b""
    buf = bytearray()
    while len(buf) < n:
        chunk = stream.read(n - len(buf))
        if not chunk:
            if not buf and allow_eof:
                return b""
            return bytes(buf)
        buf.extend(chunk)
    return bytes(buf)
