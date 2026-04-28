"""Round-trip tests for the length-prefixed JSON frame codec.

The Rust counterpart at `src-tauri/src/sidecar/framing.rs` ships its
own round-trip test against the same wire shape (4-byte big-endian
length prefix + UTF-8 body). If both pass, the two ends agree on the
binary format.
"""

from __future__ import annotations

import io
import struct

import pytest

from agent_runtime.framing import (
    FrameError,
    MAX_FRAME_BYTES,
    read_frame,
    write_frame,
)


def test_round_trip_single_frame() -> None:
    """write_frame → read_frame yields the original body bytes."""
    buf = io.BytesIO()
    body = b'{"hello":"world"}'
    write_frame(buf, body)
    buf.seek(0)
    assert read_frame(buf) == body


def test_round_trip_three_frames_back_to_back() -> None:
    """Multiple frames in a single buffer decode in order."""
    buf = io.BytesIO()
    bodies = [b'{"a":1}', b'{"b":[1,2,3]}', b'{"c":null}']
    for b in bodies:
        write_frame(buf, b)
    buf.seek(0)
    for expected in bodies:
        assert read_frame(buf) == expected
    # Fourth read returns clean EOF.
    assert read_frame(buf) == b""


def test_clean_eof_returns_empty() -> None:
    """An empty stream is a clean shutdown signal, not an error."""
    buf = io.BytesIO()
    assert read_frame(buf) == b""


def test_oversized_frame_rejected() -> None:
    """write_frame refuses bodies larger than the per-frame cap."""
    buf = io.BytesIO()
    with pytest.raises(FrameError):
        write_frame(buf, b"x" * (MAX_FRAME_BYTES + 1))


def test_truncated_body_raises() -> None:
    """A length prefix advertising more bytes than are present raises."""
    # length = 100, but only 4 body bytes follow.
    bad = struct.pack("!I", 100) + b"oops"
    buf = io.BytesIO(bad)
    with pytest.raises(FrameError):
        read_frame(buf)


def test_unicode_body_preserved() -> None:
    """UTF-8 JSON with multibyte chars round-trips verbatim."""
    buf = io.BytesIO()
    payload = '{"name":"süßer Hund 🐶"}'.encode("utf-8")
    write_frame(buf, payload)
    buf.seek(0)
    assert read_frame(buf).decode("utf-8") == '{"name":"süßer Hund 🐶"}'


def test_zero_length_frame_round_trips() -> None:
    """A frame with an empty body is a legal degenerate case."""
    buf = io.BytesIO()
    write_frame(buf, b"")
    buf.seek(0)
    assert read_frame(buf) == b""
    # Second read sees real EOF.
    assert read_frame(buf) == b""
