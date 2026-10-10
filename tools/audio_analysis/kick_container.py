"""The kick model container: named little-endian arrays (docs/KICK_REALTIME_DESIGN.md section 2).

Rust reader: crates/manifold-audio/src/kick/container.rs. Both sides must agree byte for byte.
"""
from __future__ import annotations

import struct
from pathlib import Path

import numpy as np

MAGIC = b'MKICK001'
DTYPES = {0: np.float64, 1: np.float32, 2: np.int32, 3: np.int16, 4: np.int64, 5: np.uint8}
CODES = {np.dtype(v): k for k, v in DTYPES.items()}


def text(s):
    return np.frombuffer(s.encode(), np.uint8)


def write(path, entries):
    """entries: {name: array}; arrays keep their dtype, which must be one of DTYPES."""
    out = bytearray(MAGIC + struct.pack('<I', len(entries)))
    for name, a in entries.items():
        a = np.ascontiguousarray(a)
        if a.dtype not in CODES:
            raise TypeError(f'{name}: dtype {a.dtype} is not in the container')
        n = name.encode()
        out += struct.pack('<H', len(n)) + n + struct.pack('<BB', CODES[a.dtype], a.ndim)
        out += struct.pack(f'<{a.ndim}Q', *a.shape) + a.astype(a.dtype.newbyteorder('<'), copy=False).tobytes()
    Path(path).write_bytes(bytes(out))


def read(path):
    b = Path(path).read_bytes()
    if b[:8] != MAGIC:
        raise ValueError(f'{path}: not a kick container')
    (count,), o, out = struct.unpack_from('<I', b, 8), 12, {}
    for _ in range(count):
        (n,) = struct.unpack_from('<H', b, o)
        name = b[o + 2:o + 2 + n].decode()
        o += 2 + n
        code, ndim = struct.unpack_from('<BB', b, o)
        shape = struct.unpack_from(f'<{ndim}Q', b, o + 2)
        o += 2 + 8 * ndim
        dt = np.dtype(DTYPES[code]).newbyteorder('<')
        size = int(np.prod(shape)) * dt.itemsize
        out[name] = np.frombuffer(b, dt, int(np.prod(shape)), o).reshape(shape).copy()
        o += size
    return out
