#!/usr/bin/env python3
"""Follow a landing gate or landing ceremony until its first terminal event.

This is deliberately a small, file based watcher.  ``landing_gate.py`` and
``land_branch.py`` already publish their progress to stdout and to live
transcripts; the watcher only observes those files and never signals the
watched process.
"""

from __future__ import annotations

import argparse
import codecs
import os
import re
import sys
import time
from pathlib import Path
from typing import Callable, Iterable


DEFAULT_HANG_SECONDS = 300.0
DEFAULT_POLL_SECONDS = 1.0

_RUN_RE = re.compile(
    r"^\[RUN\]\s+(?P<label>.+?)\s+\(live transcript:\s*(?P<path>.+?)\)\s*$"
)
_GATE_LOG_RE = re.compile(r"^\[land\]\s+complete landing gate transcript:\s*(?P<path>.+?)\s*$")
_GATE_SUMMARY_RE = re.compile(
    r"^landing gate:\s*(?P<passed>\d+)\s+passed,\s*"
    r"(?P<failed>\d+)\s+failed,\s*(?P<skipped>\d+)\s+skipped\s*$"
)
_STATUS_RE = re.compile(r"^\[(?:PASS|FAIL|REUSED)\]\s+(?P<label>.+?)(?:\s+\([^)]*\))?\s*$")
MAX_PARTIAL_BYTES = 64 * 1024
READ_CHUNK_BYTES = 64 * 1024


class _LogState:
    """Bounded parser state for one append-only transcript."""

    def __init__(self, label: str, path: Path, *, gate_log: bool) -> None:
        self.gate_log = gate_log
        self.context = (label, path)
        self.active: tuple[str, Path] | None = None
        self.announced = False
        self.transcripts: list[tuple[str, Path, bool]] = []
        self.failure: tuple[str, Path, tuple[str, str]] | None = None
        self.summary: tuple[bool, bool] | None = None
        self.complete = False
        self.done = False
        self._pending = ""

    def feed(self, text: str) -> None:
        self._pending += text
        lines = self._pending.splitlines(keepends=True)
        if lines and not lines[-1].endswith(("\n", "\r")):
            self._pending = lines.pop()
        else:
            self._pending = ""
        for line in lines:
            self._line(line, structural=True)
        if self._pending:
            # Markers can be emitted without a trailing newline. Structural
            # lines wait for completion so a split RUN line is never guessed.
            self._line(self._pending, structural=False)
            if len(self._pending) > MAX_PARTIAL_BYTES:
                self._pending = self._pending[-MAX_PARTIAL_BYTES:]

    def _line(self, line: str, *, structural: bool) -> None:
        plain = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', line).rstrip()
        stripped = plain.strip()
        parse_structure = structural or bool(
            _RUN_RE.match(stripped) or _GATE_LOG_RE.match(stripped)
            or _STATUS_RE.match(stripped))
        # Gate output is unindented; its quoted transcript tails are not.
        control_line = self.gate_log and plain == stripped
        if control_line and parse_structure:
            run = _RUN_RE.match(stripped)
            if run:
                context = (run.group("label").strip(),
                           Path(run.group("path").strip()))
                self.context = context
                self.active = context
                self.announced = True
                child = (*context, False)
                if child not in self.transcripts:
                    self.transcripts.append(child)
            gate_log = _GATE_LOG_RE.match(stripped)
            if gate_log:
                context = ("landing-gate", Path(gate_log.group("path").strip()))
                child = (*context, True)
                if child not in self.transcripts:
                    self.transcripts.append(child)
            if _STATUS_RE.match(stripped):
                self.active = None

        marker = (_marker(stripped, gate_log=self.gate_log)
                  if control_line or not self.gate_log else None)
        if marker is not None and self.failure is None:
            self.failure = (*self.context, marker)
        summary = _summary_line(stripped) if control_line else None
        if summary is not None:
            self.summary = summary
        if control_line and stripped == "[COMPLETE] landing gate: passed":
            self.complete = True
        if control_line and stripped.startswith("[land] DONE:"):
            self.done = True


class _IncrementalLogs:
    """Consume each transcript byte once and retain only parser state."""

    def __init__(self) -> None:
        self._offsets: dict[Path, int] = {}
        self._identities: dict[Path, tuple[int, int] | None] = {}
        self._decoders: dict[Path, codecs.IncrementalDecoder] = {}
        self._states: dict[Path, _LogState] = {}

    def read(self, path: Path, label: str, context_path: Path, *, gate_log: bool) -> _LogState:
        if path not in self._states:
            self._states[path] = _LogState(label, context_path, gate_log=gate_log)
            self._decoders[path] = codecs.getincrementaldecoder("utf-8")(
                errors="replace")
        state = self._states[path]
        try:
            stat = path.stat()
            identity = (stat.st_ino, stat.st_dev)
            offset = self._offsets.get(path, 0)
            if (self._identities.get(path) not in (None, identity)
                    or stat.st_size < offset):
                offset = 0
                state = _LogState(label, context_path, gate_log=gate_log)
                self._states[path] = state
                self._decoders[path] = codecs.getincrementaldecoder("utf-8")(
                    errors="replace")
            snapshot_end = stat.st_size
            with path.open("rb") as stream:
                stream.seek(offset)
                while offset < snapshot_end:
                    chunk = stream.read(min(READ_CHUNK_BYTES, snapshot_end - offset))
                    if not chunk:
                        break
                    offset += len(chunk)
                    state.feed(self._decoders[path].decode(chunk))
            self._identities[path] = identity
            self._offsets[path] = offset
        except (FileNotFoundError, OSError):
            pass
        return state


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


def _marker(text: str, *, gate_log: bool) -> tuple[str, str] | None:
    """Keep gate control output separate from test-runner verdicts.

    Arbitrary diagnostics (including caught/expected panics) are not runner
    verdicts. A panic becomes a red when the runner reports the test failed.
    """
    for line in text.splitlines():
        stripped = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', line).strip()
        if re.match(r"^GPU-PROOFS GATE:\s*HUNG\b", stripped):
            return "hang", stripped
        if "SubmissionsIgnored" in stripped:
            return "red", stripped
        if not gate_log:
            if (re.match(r"^test\s+\S+\s+\.\.\.\s+FAILED\b", stripped)
                    or re.match(r"^FAIL\s+\[\s*[\d.]+s\]\s+\S+", stripped)
                    or re.match(r"^test result: FAILED\.\s+\d+ passed;\s+\d+ failed;", stripped)):
                return "red", stripped
            continue
        if ("[INCOMPLETE]" in stripped or "[REFUSAL]" in stripped
                or "[REFUSED]" in stripped
                or "cancelled by SIGTERM" in stripped
                or "cancelled by SIGINT" in stripped):
            return "refusal", stripped
        if re.match(r"^(?:Storage admission refused:|(?:Automatic )?approval review(?:er)? (?:refused|rejected|blocked))", stripped, re.I):
            return "refusal", stripped
        if stripped.startswith("[land] FAILED") or "no --named-red/--reason" in stripped:
            return "red", stripped
        if stripped.startswith("[FAIL] "):
            return "red", stripped
    return None


def _summary_line(text: str) -> tuple[bool, bool] | None:
    match = _GATE_SUMMARY_RE.match(text)
    if match:
        return int(match.group("failed")) == 0, True
    return None


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
    logs = _IncrementalLogs()

    while True:
        outer_label = "landing" if kind == "land" else "landing-gate"
        known: dict[Path, tuple[str, Path, bool]] = {outer: (outer_label, outer, True)}
        sources: list[Path] = [outer]
        states: dict[Path, _LogState] = {}
        # Read every discovered transcript, including completed legs. A
        # completed transcript can contain the red that the outer log only
        # summarises as a later successful-looking step.
        index = 0
        while index < len(sources):
            path = sources[index]
            index += 1
            label, context_path, gate_log = known[path]
            states[path] = logs.read(path, label, context_path, gate_log=gate_log)
            for child_label, child_path, child_gate_log in states[path].transcripts:
                if child_path not in known:
                    known[child_path] = (child_label, child_path, child_gate_log)
                    sources.append(child_path)

        outer_state = states[outer]
        active = outer_state.active
        # The gate transcript may be the only place where RUN lines exist when
        # this watcher is attached to a land process.
        if not outer_state.announced:
            for path in sources[1:]:
                nested_active = states[path].active
                if nested_active is not None:
                    active = nested_active
                    break

        active_path = active[1] if active else outer
        active_label = active[0] if active else ("landing" if kind == "land" else "landing-gate")

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
        for path in sources:
            found = states[path].failure
            if found:
                label, transcript, marker = found
                return _event(label, marker[0], started, transcript, clock, marker[1])

        summary_path = outer
        summary = outer_state.summary
        if summary is None:
            for path in sources[1:]:
                summary = states[path].summary
                if summary is not None:
                    summary_path = path
                    break
        if summary is not None:
            green, _ = summary
            if not green:
                return _event("landing-gate", "red", started, summary_path, clock,
                              "landing gate summary has failed checks")
            if kind == "gate" and states[summary_path].complete:
                return _event("landing-gate", "none", started, outer, clock)

        if kind == "land" and outer_state.done:
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
