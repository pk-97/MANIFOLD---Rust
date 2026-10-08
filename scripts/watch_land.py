#!/usr/bin/env python3
"""Follow a landing gate or landing ceremony until its first terminal event.

This is deliberately a small, file based watcher.  ``landing_gate.py`` and
``land_branch.py`` already publish their progress to stdout and to live
transcripts; the watcher only observes those files and never signals the
watched process.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import time
from pathlib import Path
from typing import Callable, Iterable


DEFAULT_HANG_SECONDS = 300.0
DEFAULT_POLL_SECONDS = 1.0
MAX_READ_BYTES = 1024 * 1024

_RUN_RE = re.compile(
    r"^\[RUN\]\s+(?P<label>.+?)\s+\(live transcript:\s*(?P<path>.+?)\)\s*$"
)
_GATE_LOG_RE = re.compile(r"^\[land\]\s+complete landing gate transcript:\s*(?P<path>.+?)\s*$")
_GATE_SUMMARY_RE = re.compile(
    r"^landing gate:\s*(?P<passed>\d+)\s+passed,\s*"
    r"(?P<failed>\d+)\s+failed,\s*(?P<skipped>\d+)\s+skipped\s*$"
)
_STATUS_RE = re.compile(r"^\[(?:PASS|FAIL|REUSED)\]\s+(?P<label>.+?)(?:\s+\([^)]*\))?\s*$")


def _read(path: Path) -> str:
    try:
        # Logs are append-only and can span many gigabytes for a noisy build.
        # All watcher markers are emitted at the tail, so bound each poll.
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            size = stream.tell()
            stream.seek(max(0, size - MAX_READ_BYTES))
            return stream.read().decode("utf-8", errors="replace")
    except (FileNotFoundError, OSError):
        return ""


def _stat_token(path: Path) -> tuple[int, int] | None:
    try:
        stat = path.stat()
    except (FileNotFoundError, OSError):
        return None
    return stat.st_mtime_ns, stat.st_size


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _transcript_paths(text: str) -> list[Path]:
    paths: list[Path] = []
    for line in text.splitlines():
        match = _RUN_RE.match(line)
        if match:
            paths.append(Path(match.group("path").strip()))
        match = _GATE_LOG_RE.match(line)
        if match:
            paths.append(Path(match.group("path").strip()))
    return paths


def _active_leg(text: str) -> tuple[str, Path] | None:
    """Return the most recently announced leg and its live transcript."""
    active, _ = _scan_active(text)
    return active


def _scan_active(text: str) -> tuple[tuple[str, Path] | None, bool]:
    """Return the current leg and whether this log has announced any leg."""
    active: tuple[str, Path] | None = None
    announced = False
    for line in text.splitlines():
        match = _RUN_RE.match(line)
        if match:
            announced = True
            active = (match.group("label").strip(), Path(match.group("path").strip()))
            continue
        if active is not None and _STATUS_RE.match(line.strip()):
            active = None
    return active, announced


def _has_status(text: str) -> bool:
    return any(_STATUS_RE.match(line.strip()) for line in text.splitlines())


def _marker(text: str) -> tuple[str, str] | None:
    """Classify the first terminal marker found in a log."""
    for line in text.splitlines():
        stripped = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', line).strip()
        if ("[INCOMPLETE]" in stripped or "[REFUSAL]" in stripped
                or "[REFUSED]" in stripped
                or "cancelled by SIGTERM" in stripped
                or "cancelled by SIGINT" in stripped):
            return "refusal", stripped
        if re.match(r"^(?:Storage admission refused:|(?:Automatic )?approval review(?:er)? (?:refused|rejected|blocked))", stripped, re.I):
            return "refusal", stripped
        if stripped.startswith("[land] FAILED") or "no --named-red/--reason" in stripped:
            return "red", stripped
        if re.search(r"(?:^|\s)\[FAIL\](?:\s|$)", stripped):
            return "red", stripped
        if re.match(r"^(?:FAIL(?:\s|\[)|FAILED\b)", stripped):
            return "red", stripped
        if re.match(r"^test\s+\S+\s+\.\.\.\s+FAILED\b", stripped):
            return "red", stripped
        if re.match(r"^GPU-PROOFS GATE:\s*HUNG\b", stripped):
            return "hang", stripped
        if re.search(r"\btest result:\s+FAILED\b", stripped):
            return "red", stripped
        if re.search(r"\bGPU-PROOFS GATE:\s*FAIL\b", stripped):
            return "red", stripped
        if re.match(r"^error(?:\[[^]]+\])?:\s*", stripped):
            return "red", stripped
    return None


def _failure_context(text, label, path):
    """Associate an outer-log failure with its leg, even after the next RUN."""
    for line in text.splitlines():
        run = _RUN_RE.match(line)
        if run:
            label, path = run['label'].strip(), Path(run['path'].strip())
        marker = _marker(line)
        if marker:
            return label, path, marker
    return None


def _summary(text: str) -> tuple[bool, bool] | None:
    """Return ``(is_green, is_summary)`` for the gate summary, if present."""
    for line in text.splitlines():
        match = _GATE_SUMMARY_RE.match(line.strip())
        if match:
            return int(match.group("failed")) == 0, True
    return None


def _done(text: str) -> bool:
    return any(line.startswith("[land] DONE:") for line in text.splitlines())


def _event(leg: str, failure: str, started: float, transcript: Path,
           clock: Callable[[], float], detail: str | None = None) -> int:
    elapsed = max(0.0, clock() - started)
    detail_field = ""
    if detail:
        clean_detail = re.sub(r"\s+", " ", detail).strip()
        detail_field = f" detail={clean_detail}"
    print(
        f"[watch] leg={leg} failure={failure}{detail_field} elapsed={elapsed:.2f}s "
        f"transcript={transcript}",
        flush=True,
    )
    return 0 if failure == "none" else 1


def watch(
    pid: int,
    log_path: Path | str,
    hang_seconds: float = DEFAULT_HANG_SECONDS,
    poll_seconds: float = DEFAULT_POLL_SECONDS,
    kind: str = "gate",
    *,
    clock: Callable[[], float] = time.monotonic,
    sleeper: Callable[[float], None] = time.sleep,
    alive: Callable[[int], bool] = _alive,
) -> int:
    """Watch ``pid`` and return zero only when the requested operation passes.

    ``clock``, ``sleeper`` and ``alive`` are injectable so fake process tests do
    not need to wait or depend on platform process tables.
    """
    if kind not in {"gate", "land"}:
        raise ValueError(f"unknown watch kind: {kind}")
    if hang_seconds < 0 or poll_seconds <= 0:
        raise ValueError("hang-seconds must be non-negative and poll-seconds positive")

    outer = Path(log_path)
    started = clock()
    last_activity = started
    current_active: tuple[str, Path] | None = None
    previous_active_token: tuple[int, int] | None = None

    while True:
        outer_text = _read(outer)
        sources: list[Path] = [outer]
        # A land log points at the gate log.  Include it in the observed set so
        # an active leg that has not yet been forwarded to stdout is visible.
        for path in _transcript_paths(outer_text):
            if path not in sources:
                sources.append(path)

        source_text: dict[Path, str] = {path: _read(path) for path in sources}
        active, outer_announced = _scan_active(outer_text)
        # The gate transcript may be the only place where RUN lines exist when
        # this watcher is attached to a land process.
        if not outer_announced and current_active is not None and not _has_status(outer_text):
            # A bounded tail can omit the RUN line after a very noisy step;
            # retain the known active leg until a status or newer RUN arrives.
            active = current_active
        elif not outer_announced:
            for path in sources[1:]:
                nested_active, _ = _scan_active(source_text[path])
                if nested_active is not None:
                    active = nested_active
                    break

        active_path = active[1] if active else outer
        active_label = active[0] if active else ("landing" if kind == "land" else "landing-gate")
        active_text = _read(active_path) if active_path not in source_text else source_text[active_path]

        now = clock()
        if active != current_active:
            current_active = active
            previous_active_token = None
            last_activity = now
        active_token = _stat_token(active_path)
        if active_token != previous_active_token:
            previous_active_token = active_token
            last_activity = now

        # Red and refusal are terminal immediately, including the cancellation
        # marker emitted by a signal-interrupted gate.
        for text in (outer_text, active_text):
            found = _failure_context(text, active_label, active_path)
            if found:
                label, path, marker = found
                return _event(label, marker[0], started, path, clock, marker[1])

        summary = _summary(outer_text)
        if summary is None:
            for path in sources[1:]:
                summary = _summary(source_text[path])
                if summary is not None:
                    break
        if summary is not None:
            green, _ = summary
            if not green:
                return _event("landing-gate", "red", started, outer, clock, "landing gate summary has failed checks")
            if kind == "gate":
                return _event("landing-gate", "none", started, outer, clock)

        if kind == "land" and _done(outer_text):
            return _event("landing", "none", started, outer, clock)

        if not alive(pid):
            return _event(active_label, "unexpected-exit", started, active_path, clock)
        now = clock()
        if now - last_activity >= hang_seconds:
            return _event(active_label, "hang", started, active_path, clock)
        sleeper(poll_seconds)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--pid", required=True, type=int)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--hang-seconds", type=float, default=DEFAULT_HANG_SECONDS)
    parser.add_argument("--poll-seconds", type=float, default=DEFAULT_POLL_SECONDS)
    parser.add_argument("--kind", choices=("gate", "land"), default="gate")
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        return watch(args.pid, args.log, args.hang_seconds, args.poll_seconds, args.kind)
    except (OSError, ValueError) as exc:
        print(f"watch_land: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
