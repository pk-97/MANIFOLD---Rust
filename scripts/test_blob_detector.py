#!/usr/bin/env python3
"""Regression tests for the legacy (V1) BlobDetector C ABI.

The fixture is intentionally small and deterministic so it can be run against
the bundled native plugin without any third-party Python dependencies.
"""

from __future__ import annotations

import argparse
import ctypes
from pathlib import Path
import sys
import unittest


WIDTH = 320
HEIGHT = 180
MAX_BLOBS = 8
THRESHOLD = 0.65
SENSITIVITY = 0.85


class NativePluginError(RuntimeError):
    """Raised when the requested native plugin cannot be used."""


def default_library() -> Path:
    return (
        Path(__file__).resolve().parents[1]
        / "assets"
        / "plugins"
        / "BlobDetector.bundle"
        / "Contents"
        / "MacOS"
        / "BlobDetector"
    )


def resolve_library(value: str | None) -> Path:
    path = Path(value) if value else default_library()
    if path.is_dir():
        path = path / "Contents" / "MacOS" / "BlobDetector"
    return path


class NativeDetector:
    """Small ctypes wrapper for the V1 ABI used by the Rust runtime."""

    def __init__(self, library_path: Path):
        if not library_path.is_file():
            raise NativePluginError(f"native plugin not found: {library_path}")
        try:
            library = ctypes.CDLL(str(library_path))
        except OSError as exc:
            raise NativePluginError(
                f"could not load native plugin {library_path}: {exc}"
            ) from exc

        try:
            library.BlobDetector_Create.argtypes = [ctypes.c_int]
            library.BlobDetector_Create.restype = ctypes.c_void_p
            library.BlobDetector_Destroy.argtypes = [ctypes.c_void_p]
            library.BlobDetector_Destroy.restype = None
            library.BlobDetector_Process.argtypes = [
                ctypes.c_void_p,
                ctypes.POINTER(ctypes.c_uint8),
                ctypes.c_int,
                ctypes.c_int,
                ctypes.c_float,
                ctypes.c_float,
                ctypes.POINTER(ctypes.c_float),
            ]
            library.BlobDetector_Process.restype = ctypes.c_int
        except AttributeError as exc:
            raise NativePluginError(
                f"native plugin is missing the V1 BlobDetector ABI: {library_path}"
            ) from exc

        handle = library.BlobDetector_Create(MAX_BLOBS)
        if not handle:
            raise NativePluginError("BlobDetector_Create returned a null handle")
        self.library = library
        self.handle = handle

    def close(self) -> None:
        if self.handle:
            self.library.BlobDetector_Destroy(self.handle)
            self.handle = None

    def __enter__(self) -> NativeDetector:
        return self

    def __exit__(self, _type: object, _value: object, _traceback: object) -> None:
        self.close()

    def process(self, rgba: bytes) -> list[tuple[float, float, float, float]]:
        if len(rgba) != WIDTH * HEIGHT * 4:
            raise ValueError(f"expected {WIDTH}x{HEIGHT} RGBA input")
        pixels = (ctypes.c_uint8 * len(rgba)).from_buffer_copy(rgba)
        output = (ctypes.c_float * (MAX_BLOBS * 4))()
        count = self.library.BlobDetector_Process(
            self.handle,
            pixels,
            WIDTH,
            HEIGHT,
            ctypes.c_float(THRESHOLD),
            ctypes.c_float(SENSITIVITY),
            output,
        )
        if count < 0 or count > MAX_BLOBS:
            raise NativePluginError(f"BlobDetector_Process returned invalid count {count}")
        return [tuple(float(output[index * 4 + offset]) for offset in range(4)) for index in range(count)]


def checker_scene() -> bytes:
    rectangles = ((50, 89, 50, 89), (210, 249, 100, 139))
    data = bytearray(WIDTH * HEIGHT * 4)
    for y in range(HEIGHT):
        for x in range(WIDTH):
            value = 20 + 8 * ((x // 4 + y // 4) % 2)
            for left, right, top, bottom in rectangles:
                if left <= x <= right and top <= y <= bottom:
                    value = 205
                    break
            offset = (y * WIDTH + x) * 4
            data[offset : offset + 4] = bytes((value, value, value, 255))
    return bytes(data)


def near_flat_scene() -> bytes:
    data = bytearray(WIDTH * HEIGHT * 4)
    for y in range(HEIGHT):
        for x in range(WIDTH):
            value = 80 + ((x * 17 + y * 31) % 3) - 1
            offset = (y * WIDTH + x) * 4
            data[offset : offset + 4] = bytes((value, value, value, 255))
    return bytes(data)


class BlobDetectorV1Regression(unittest.TestCase):
    detector: NativeDetector

    @classmethod
    def setUpClass(cls) -> None:
        cls.detector = NativeDetector(LIBRARY_PATH)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.detector.close()

    def test_textured_scene_keeps_two_strong_regions(self) -> None:
        regions = self.detector.process(checker_scene())
        self.assertEqual(len(regions), 2, regions)

        expected = (
            (70 / WIDTH, 1.0 - 70 / HEIGHT, 50 / WIDTH, 50 / HEIGHT, 50, 89, 50, 89),
            (230 / WIDTH, 1.0 - 120 / HEIGHT, 50 / WIDTH, 50 / HEIGHT, 210, 249, 100, 139),
        )
        unmatched = list(expected)
        for cx, cy, width, height in regions:
            self.assertGreaterEqual(cx, 0.0)
            self.assertLessEqual(cx, 1.0)
            self.assertGreaterEqual(cy, 0.0)
            self.assertLessEqual(cy, 1.0)
            self.assertGreater(width, 0.0)
            self.assertGreater(height, 0.0)
            self.assertLess(width * height, 0.3)

            match = min(unmatched, key=lambda item: abs(cx - item[0]) + abs(cy - item[1]))
            unmatched.remove(match)
            expected_cx, expected_cy, expected_width, expected_height, left, right, top, bottom = match
            self.assertAlmostEqual(cx, expected_cx, delta=0.04)
            self.assertAlmostEqual(cy, expected_cy, delta=0.04)
            self.assertAlmostEqual(width, expected_width, delta=0.05)
            self.assertAlmostEqual(height, expected_height, delta=0.06)

            box_left = cx - width / 2.0
            box_right = cx + width / 2.0
            box_top = 1.0 - cy - height / 2.0
            box_bottom = 1.0 - cy + height / 2.0
            self.assertLessEqual(box_left, left / WIDTH + 0.01)
            self.assertGreaterEqual(box_right, (right + 1) / WIDTH - 0.01)
            self.assertLessEqual(box_top, top / HEIGHT + 0.01)
            self.assertGreaterEqual(box_bottom, (bottom + 1) / HEIGHT - 0.01)

    def test_near_flat_frame_rejects_noise(self) -> None:
        self.assertEqual(self.detector.process(near_flat_scene()), [])


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--library",
        metavar="PATH",
        help="path to BlobDetector.bundle or its macOS executable (default: bundled plugin)",
    )
    args = parser.parse_args(argv)

    global LIBRARY_PATH
    LIBRARY_PATH = resolve_library(args.library)
    try:
        NativeDetector(LIBRARY_PATH).close()
    except NativePluginError as exc:
        parser.error(str(exc))

    suite = unittest.defaultTestLoader.loadTestsFromTestCase(BlobDetectorV1Regression)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


LIBRARY_PATH = default_library()


if __name__ == "__main__":
    sys.exit(main())
