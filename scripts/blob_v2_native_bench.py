#!/usr/bin/env python3
"""Measure the bundled BlobDetector V2 C ABI.

The default run measures four deterministic fixtures at 320x180 with an
eight-region output cap, followed by the same fixtures at 1024x1024 with the
32-region cap.  Input and output buffers are allocated once per fixture and
reused for warmup and timed calls.

The benchmark measures native-call wall time from Python.  It does not count
transient allocation events.  A malloc-zone result is a net live allocation
snapshot (and includes the Python harness); it is never an allocation-event
count.  When malloc-zone statistics are unavailable, peak process RSS is
reported instead.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import math
import os
from pathlib import Path
import platform
import resource
import sys
import time
from dataclasses import dataclass
from typing import Any, Callable, Iterable


MAX_REGIONS = 32
DEFAULT_WARMUP = 20
DEFAULT_ITERATIONS = 100


class BlobRegionV2(ctypes.Structure):
    _fields_ = [
        ("label", ctypes.c_uint32),
        ("x", ctypes.c_float),
        ("y", ctypes.c_float),
        ("width", ctypes.c_float),
        ("height", ctypes.c_float),
        ("area", ctypes.c_float),
        ("cx", ctypes.c_float),
        ("cy", ctypes.c_float),
    ]


class BlobRegionOptionsV2(ctypes.Structure):
    _fields_ = [
        ("threshold", ctypes.c_float),
        ("min_area", ctypes.c_float),
        ("max_area", ctypes.c_float),
        ("min_aspect", ctypes.c_float),
        ("max_aspect", ctypes.c_float),
        ("max_regions", ctypes.c_uint32),
    ]


assert ctypes.sizeof(BlobRegionV2) == 32
assert ctypes.sizeof(BlobRegionOptionsV2) == 24
assert BlobRegionV2.label.offset == 0
assert BlobRegionV2.x.offset == 4
assert BlobRegionV2.cy.offset == 28


class NativeError(RuntimeError):
    """Raised when the native ABI cannot be loaded or returns invalid data."""


@dataclass(frozen=True)
class Fixture:
    name: str
    build: Callable[[int, int], bytearray]
    expected_components: Callable[[int, int, int], int]


@dataclass(frozen=True)
class MemorySnapshot:
    method: str
    bytes_in_use: int | None
    blocks_in_use: int | None
    peak_rss_bytes: int | None
    note: str


@dataclass(frozen=True)
class TimingSummary:
    median_ms: float
    p95_ms: float
    max_ms: float


@dataclass(frozen=True)
class BenchmarkResult:
    suite: str
    fixture: str
    width: int
    height: int
    max_regions: int
    expected_count: int
    count: int
    timing: TimingSummary


def _library_default() -> Path:
    root = Path(__file__).resolve().parents[1]
    return root / "assets/plugins/BlobDetector.bundle/Contents/MacOS/BlobDetector"


def _resolve_library(value: str | None) -> Path:
    candidate = Path(value or os.environ.get("MANIFOLD_BLOBDETECTOR_PLUGIN", _library_default()))
    if candidate.is_dir():
        candidate = candidate / "Contents/MacOS/BlobDetector"
    return candidate


class NativeDetector:
    def __init__(self, library_path: Path):
        if not library_path.exists():
            raise NativeError(f"native library does not exist: {library_path}")
        try:
            self.library = ctypes.CDLL(str(library_path))
        except OSError as exc:
            raise NativeError(f"could not load {library_path}: {exc}") from exc

        self.library.BlobDetectorV2_Create.argtypes = []
        self.library.BlobDetectorV2_Create.restype = ctypes.c_void_p
        self.library.BlobDetectorV2_Destroy.argtypes = [ctypes.c_void_p]
        self.library.BlobDetectorV2_Destroy.restype = None
        self.library.BlobDetectorV2_Process.argtypes = [
            ctypes.c_void_p,
            ctypes.POINTER(ctypes.c_uint8),
            ctypes.c_size_t,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.POINTER(BlobRegionOptionsV2),
            ctypes.POINTER(ctypes.c_uint8),
            ctypes.c_size_t,
            ctypes.POINTER(BlobRegionV2),
            ctypes.c_size_t,
        ]
        self.library.BlobDetectorV2_Process.restype = ctypes.c_int32
        self.handle = self.library.BlobDetectorV2_Create()
        if not self.handle:
            raise NativeError("BlobDetectorV2_Create returned null")

    def close(self) -> None:
        if self.handle:
            self.library.BlobDetectorV2_Destroy(self.handle)
            self.handle = None

    def __enter__(self) -> "NativeDetector":
        return self

    def __exit__(self, _type: Any, _value: Any, _traceback: Any) -> None:
        self.close()

    def process(
        self,
        rgba: Any,
        rgba_len: int,
        width: int,
        height: int,
        options: BlobRegionOptionsV2,
        labels: Any,
        labels_len: int,
        regions: Any,
        regions_capacity: int,
    ) -> int:
        result = self.library.BlobDetectorV2_Process(
            self.handle,
            rgba,
            rgba_len,
            width,
            height,
            ctypes.byref(options),
            labels,
            labels_len,
            regions,
            regions_capacity,
        )
        if result < 0:
            raise NativeError(f"BlobDetectorV2_Process returned {result}")
        return int(result)


def _blank(width: int, height: int) -> bytearray:
    data = bytearray(width * height * 4)
    for pixel in range(width * height):
        data[pixel * 4 + 3] = 255
    return data


def _ring(width: int, height: int) -> bytearray:
    data = _blank(width, height)
    cx, cy = width / 2.0, height / 2.0
    outer = min(width, height) * 0.30
    inner = outer * 0.54
    outer_sq, inner_sq = outer * outer, inner * inner
    for y in range(height):
        dy = y + 0.5 - cy
        for x in range(width):
            dx = x + 0.5 - cx
            distance_sq = dx * dx + dy * dy
            if inner_sq <= distance_sq <= outer_sq:
                data[(y * width + x) * 4] = 255
    return data


def _speckles(width: int, height: int) -> bytearray:
    """A fixed grid of isolated one-pixel components, used as noisy input."""
    data = _blank(width, height)
    columns = 16
    rows = 8
    x_step = max(3, width // (columns + 1))
    y_step = max(3, height // (rows + 1))
    for row in range(rows):
        y = min(height - 1, y_step * (row + 1))
        for column in range(columns):
            x = min(width - 1, x_step * (column + 1))
            data[(y * width + x) * 4] = 255
    return data


def _crowded(width: int, height: int) -> bytearray:
    """A dense, deterministic grid of separated 2x2 components."""
    data = _blank(width, height)
    columns = 16
    rows = 8
    x_step = max(4, width // (columns + 1))
    y_step = max(4, height // (rows + 1))
    for row in range(rows):
        y = min(height - 2, y_step * (row + 1))
        for column in range(columns):
            x = min(width - 2, x_step * (column + 1))
            for dy in range(2):
                for dx in range(2):
                    data[((y + dy) * width + x + dx) * 4] = 255
    return data


FIXTURES = (
    Fixture("empty", _blank, lambda _width, _height, _cap: 0),
    Fixture("ring", _ring, lambda _width, _height, _cap: 1),
    Fixture("noisy", _speckles, lambda _width, _height, cap: min(128, cap)),
    Fixture("crowded", _crowded, lambda _width, _height, cap: min(128, cap)),
)


def _options(max_regions: int) -> BlobRegionOptionsV2:
    # Wide area/aspect bounds keep the fixture count independent of normalized
    # dimensions while still exercising the native filtering path.
    return BlobRegionOptionsV2(
        threshold=0.5,
        min_area=0.0,
        max_area=1.0,
        min_aspect=0.0,
        max_aspect=100.0,
        max_regions=max_regions,
    )


def _validate_outputs(
    labels: Any,
    regions: Any,
    width: int,
    height: int,
    max_regions: int,
    expected_count: int,
    result: int,
) -> None:
    if not 0 <= result <= max_regions:
        raise NativeError(f"fixture returned {result}; expected 0..{max_regions}")
    if result != expected_count:
        raise NativeError(
            f"fixture count mismatch at {width}x{height}: returned {result}, expected {expected_count}"
        )
    seen = set()
    for index in range(width * height):
        label = int(labels[index])
        if label > result:
            raise NativeError(f"label {label} exceeds selected count {result}")
        if label:
            seen.add(label)
    expected_labels = set(range(1, result + 1))
    if seen != expected_labels:
        raise NativeError(f"labels were {sorted(seen)}, expected {sorted(expected_labels)}")
    region_labels = {int(regions[index].label) for index in range(result)}
    if region_labels != expected_labels:
        raise NativeError(
            f"region labels were {sorted(region_labels)}, expected {sorted(expected_labels)}"
        )


def _nearest_rank(values: Iterable[float], percentile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("cannot calculate a percentile of no timings")
    index = max(0, min(len(ordered) - 1, math.ceil(percentile * len(ordered)) - 1))
    return ordered[index]


def _timing(values_ns: list[int]) -> TimingSummary:
    return TimingSummary(
        median_ms=_nearest_rank(values_ns, 0.50) / 1_000_000.0,
        p95_ms=_nearest_rank(values_ns, 0.95) / 1_000_000.0,
        max_ms=max(values_ns) / 1_000_000.0,
    )


def _peak_rss_bytes() -> int | None:
    try:
        value = int(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)
    except (AttributeError, OSError, ValueError):
        return None
    # Darwin reports bytes; Linux and the other common Unix implementations
    # report KiB.  This is only a fallback when malloc-zone statistics fail.
    return value if sys.platform == "darwin" else value * 1024


class _MallocStatistics(ctypes.Structure):
    _fields_ = [
        ("blocks_in_use", ctypes.c_uint64),
        ("size_in_use", ctypes.c_uint64),
        ("max_size_in_use", ctypes.c_uint64),
        ("size_allocated", ctypes.c_uint64),
    ]


def _malloc_snapshot() -> MemorySnapshot | None:
    if platform.system() != "Darwin":
        return None
    try:
        libc = ctypes.CDLL(None)
        default_zone = libc.malloc_default_zone
        statistics = libc.malloc_zone_statistics
        default_zone.argtypes = []
        default_zone.restype = ctypes.c_void_p
        statistics.argtypes = [ctypes.c_void_p, ctypes.POINTER(_MallocStatistics)]
        statistics.restype = None
        zone = default_zone()
        if not zone:
            return None
        result = _MallocStatistics()
        statistics(zone, ctypes.byref(result))
        return MemorySnapshot(
            method="malloc_zone_statistics",
            bytes_in_use=int(result.size_in_use),
            blocks_in_use=int(result.blocks_in_use),
            peak_rss_bytes=None,
            note="default malloc zone net live snapshot; includes Python harness",
        )
    except (AttributeError, OSError, TypeError, ValueError):
        return None


def _memory_snapshot() -> MemorySnapshot:
    snapshot = _malloc_snapshot()
    if snapshot is not None:
        return snapshot
    return MemorySnapshot(
        method="peak_rss",
        bytes_in_use=None,
        blocks_in_use=None,
        peak_rss_bytes=_peak_rss_bytes(),
        note="peak process RSS; includes Python harness and loader",
    )


def _memory_delta(before: MemorySnapshot, after: MemorySnapshot) -> dict[str, Any]:
    if before.method == after.method == "malloc_zone_statistics":
        return {
            "method": after.method,
            "retained_bytes_delta": int(after.bytes_in_use or 0) - int(before.bytes_in_use or 0),
            "retained_blocks_delta": int(after.blocks_in_use or 0) - int(before.blocks_in_use or 0),
            "note": after.note,
        }
    before_rss = before.peak_rss_bytes
    after_rss = after.peak_rss_bytes
    return {
        "method": "peak_rss",
        "peak_rss_bytes": after_rss,
        "peak_rss_delta": None if before_rss is None or after_rss is None else after_rss - before_rss,
        "note": after.note,
    }


def _benchmark_fixture(
    detector: NativeDetector,
    suite: str,
    fixture: Fixture,
    width: int,
    height: int,
    max_regions: int,
    warmup: int,
    iterations: int,
) -> BenchmarkResult:
    if width < 1 or height < 1 or max_regions < 1 or max_regions > MAX_REGIONS:
        raise ValueError("invalid benchmark dimensions or region cap")
    data = fixture.build(width, height)
    rgba = (ctypes.c_uint8 * len(data)).from_buffer(data)
    labels = (ctypes.c_uint8 * (width * height))()
    regions = (BlobRegionV2 * MAX_REGIONS)()
    options = _options(max_regions)
    expected_count = fixture.expected_components(width, height, max_regions)

    def call() -> int:
        return detector.process(
            rgba,
            len(data),
            width,
            height,
            options,
            labels,
            width * height,
            regions,
            MAX_REGIONS,
        )

    for _ in range(warmup):
        call()
    result = call()
    _validate_outputs(labels, regions, width, height, max_regions, expected_count, result)

    timings: list[int] = []
    for _ in range(iterations):
        start = time.perf_counter_ns()
        result = call()
        timings.append(time.perf_counter_ns() - start)
    _validate_outputs(labels, regions, width, height, max_regions, expected_count, result)
    return BenchmarkResult(
        suite=suite,
        fixture=fixture.name,
        width=width,
        height=height,
        max_regions=max_regions,
        expected_count=expected_count,
        count=result,
        timing=_timing(timings),
    )


def _run(args: argparse.Namespace) -> tuple[list[BenchmarkResult], dict[str, Any]]:
    before = _memory_snapshot()
    results: list[BenchmarkResult] = []
    library_path = _resolve_library(args.library)
    with NativeDetector(library_path) as detector:
        for fixture in FIXTURES:
            results.append(
                _benchmark_fixture(
                    detector,
                    "default",
                    fixture,
                    320,
                    180,
                    8,
                    args.warmup,
                    args.iterations,
                )
            )
        if not args.skip_stress:
            for fixture in FIXTURES:
                results.append(
                    _benchmark_fixture(
                        detector,
                        "stress",
                        fixture,
                        1024,
                        1024,
                        32,
                        args.warmup,
                        args.iterations,
                    )
                )
        # Capture while the reusable native state and its OpenCV buffers are
        # still alive.  The destructor below intentionally is not part of the
        # retained-memory measurement.
        after = _memory_snapshot()
    return results, _memory_delta(before, after)


def _result_dict(result: BenchmarkResult) -> dict[str, Any]:
    return {
        "suite": result.suite,
        "fixture": result.fixture,
        "width": result.width,
        "height": result.height,
        "max_regions": result.max_regions,
        "expected_count": result.expected_count,
        "count": result.count,
        "median_ms": result.timing.median_ms,
        "p95_ms": result.timing.p95_ms,
        "max_ms": result.timing.max_ms,
    }


def _print_report(
    results: list[BenchmarkResult], memory: dict[str, Any], warmup: int, iterations: int
) -> None:
    print("BlobDetector V2 native ABI benchmark")
    print(f"warmup={warmup} timed_calls={iterations} (buffers reused per fixture)")
    print("timings are native Process calls measured with Python perf_counter_ns")
    print("fixture                 size       cap  count  median_ms  p95_ms  max_ms")
    for result in results:
        print(
            f"{result.suite + '/' + result.fixture:<23} "
            f"{result.width:4}x{result.height:<4} {result.max_regions:5} "
            f"{result.count:5} {result.timing.median_ms:10.3f} "
            f"{result.timing.p95_ms:7.3f} {result.timing.max_ms:7.3f}"
        )
    print(f"memory: {json.dumps(memory, sort_keys=True)}")
    print("allocation events: UNMEASURED (net live blocks are not event counts)")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--library",
        help="path to BlobDetector.bundle or its macOS executable (default: bundled plugin)",
    )
    parser.add_argument("--warmup", type=int, default=DEFAULT_WARMUP, help="warmup calls per fixture")
    parser.add_argument(
        "--iterations", type=int, default=DEFAULT_ITERATIONS, help="timed calls per fixture"
    )
    parser.add_argument(
        "--skip-stress", action="store_true", help="skip the separate 1024x1024 cap-32 suite"
    )
    parser.add_argument("--json", action="store_true", help="emit machine-readable JSON only")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if args.warmup < 0 or args.iterations < 1:
        raise SystemExit("--warmup must be >= 0 and --iterations must be >= 1")
    try:
        results, memory = _run(args)
    except (NativeError, OSError, ValueError) as exc:
        print(f"blob_v2_native_bench: error: {exc}", file=sys.stderr)
        return 2
    if args.json:
        print(
            json.dumps(
                {
                    "warmup": args.warmup,
                    "iterations": args.iterations,
                    "results": [_result_dict(result) for result in results],
                    "memory": memory,
                    "allocation_events": "unmeasured",
                },
                sort_keys=True,
            )
        )
    else:
        _print_report(results, memory, args.warmup, args.iterations)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
