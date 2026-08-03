#!/usr/bin/env python3
"""Census report of hook-telemetry records.

Reads .claude/telemetry/hook-fires.jsonl and .claude/settings.json from the
repo root (resolved from __file__), and prints:

  - Per-hook table: fires, acted (out>0 or exit!=0), blocks (deny/ask/block),
    sorted by fires descending.
  - Hooks registered in settings.json that never fired in the window.
  - Hooks that fired but never acted in the window.

--days N      filter to records within the last N days (default 14)
--strict      exit 1 when any registered hook never fired

The telemetry log is gitignored via .claude/* — absent is not an error; the
script prints an empty report and exits 0.
"""

import argparse
import json
import sys
from datetime import datetime, timezone, timedelta
from pathlib import Path


def _find_repo_root() -> Path:
    """Walk up from __file__ to find the checkout root with .claude/settings.json.

    Handles both the main checkout and git worktrees (where .claude is not
    tracked and may or may not exist).
    """
    start = Path(__file__).resolve().parent
    for p in [start] + list(start.parents):
        if (p / ".claude" / "settings.json").is_file():
            return p
    # Fallback: scripts/ is tracked, so parent of scripts/ is the root.
    return start.parent if start.name == "scripts" else start


_REPO = _find_repo_root()
TELEMETRY = _REPO / ".claude" / "telemetry" / "hook-fires.jsonl"
SETTINGS = _REPO / ".claude" / "settings.json"


def _parse_ts(raw: str) -> datetime | None:
    """Parse an ISO-8601 timestamp, tolerating trailing Z and +HH:MM zones."""
    # Python <3.11 can't parse +00:00 with fromisoformat; strip zone via rsplit.
    try:
        raw = raw.strip()
        # Handle trailing Z
        if raw.endswith("Z"):
            raw = raw[:-1] + "+00:00"
        # Strip timezone offset for parsing, add back as UTC
        # "2026-07-28T06:37:22+00:00" — split on + or -, take first part
        if "+" in raw[10:] or "-" in raw[10:]:
            # Find the last + or - that starts a timezone offset
            # ISO format: YYYY-MM-DDTHH:MM:SS±HH:MM
            # The T separator at index 10 splits date from time
            # Find the timezone marker: typically at position 19
            if len(raw) > 19 and raw[19] in ("+", "-"):
                return datetime.fromisoformat(raw)
            # For +HH:MM offsets after seconds
            # +00:00: length = 25, position 19 is the +/-
            if len(raw) >= 19:
                sign_pos = -1
                for i in range(19, len(raw)):
                    if raw[i] in ("+", "-") and i > 19:
                        sign_pos = i
                        break
                if sign_pos > 0:
                    dt_str = raw[:sign_pos]
                    return datetime.fromisoformat(dt_str).replace(tzinfo=timezone.utc)
        return datetime.fromisoformat(raw)
    except (ValueError, TypeError):
        return None


def load_records(path: Path, days: int) -> tuple[list[dict], int]:
    """Load telemetry records within the window. Returns (records, total_seen)."""
    if not path.exists():
        return [], 0

    cutoff = datetime.now(timezone.utc) - timedelta(days=days)
    records = []
    total = 0
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            total += 1
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            ts = _parse_ts(rec.get("ts", ""))
            if ts is not None and ts >= cutoff:
                records.append(rec)
    return records, total


def extract_hook_names_from_settings(path: Path) -> set[str]:
    """Parse settings.json and return set of hook filenames that should fire."""
    if not path.exists():
        return set()

    with open(path) as f:
        settings = json.load(f)

    names: set[str] = set()
    hooks_config = settings.get("hooks", {})

    for event_name, matcher_groups in hooks_config.items():
        if not isinstance(matcher_groups, list):
            continue
        for group in matcher_groups:
            hooks_list = group.get("hooks", [])
            if not isinstance(hooks_list, list):
                continue
            for entry in hooks_list:
                command = entry.get("command", "")
                name = _extract_hook_name(command)
                if name:
                    names.add(name)

    return names


def _extract_hook_name(command: str) -> str | None:
    """Extract hook filename from a command string.

    Handles:
      python3 "...hook_telemetry.py" preToolUseBash.py
      python3 "...direct-hook.py"
    """
    parts = command.strip().split()
    # Last .py arg that is not hook_telemetry.py is the hook name.
    # If no wrapped hook is found, look for any .py as a direct hook.
    candidates = [p for p in parts if p.endswith(".py")]
    for c in reversed(candidates):
        name = c.strip("\"'").split("/")[-1]
        if name != "hook_telemetry.py":
            return name
    return None


def per_hook_stats(records: list[dict]) -> list[tuple[str, int, int, int]]:
    """Aggregate stats per hook file.

    Returns list of (hook_name, fires, acted, blocks) sorted by fires desc.
    """
    stats: dict[str, list[int]] = {}  # hook -> [fires, acted, blocks]
    for rec in records:
        hook = rec.get("hook", "?")
        if hook not in stats:
            stats[hook] = [0, 0, 0]
        stats[hook][0] += 1  # fires

        out = rec.get("out", 0)
        exit_code = rec.get("exit", 0)
        if isinstance(out, (int, float)) and (out > 0 or exit_code != 0):
            stats[hook][1] += 1  # acted

        decision = rec.get("decision")
        if isinstance(decision, str) and decision in ("deny", "ask", "block"):
            stats[hook][2] += 1  # blocks

    result = [(name, fires, acted, blocks)
              for name, (fires, acted, blocks) in stats.items()]
    result.sort(key=lambda x: -x[1])
    return result


def report(records: list[dict], registered: set[str], days: int, strict: bool) -> int:
    """Print the census report. Returns exit code."""
    stats = per_hook_stats(records)

    print(f"=== Hook activity (last {days} days) ===")
    print()
    print(f"{'Fires':>8} {'Acted':>8} {'Blocks':>8}  Hook")
    print(f"{'------':>8} {'------':>8} {'------':>8}  ----")
    for hook, fires, acted_, blocks in stats:
        print(f"{fires:>8} {acted_:>8} {blocks:>8}  {hook}")
    print()

    fired_hooks = {s[0] for s in stats}

    never_fired = sorted(registered - fired_hooks)
    if never_fired:
        print(f"REGISTERED BUT NEVER FIRED ({len(never_fired)}):")
        for h in never_fired:
            print(f"  - {h}")
        print()
    else:
        print("REGISTERED BUT NEVER FIRED: none")
        print()

    never_acted = sorted(h for h, _, acted_, _ in stats if acted_ == 0)
    if never_acted:
        print(f"NEVER ACTED ({len(never_acted)}):")
        for h in never_acted:
            print(f"  - {h}")
        print()
    else:
        print("NEVER ACTED: none")
        print()

    if strict and never_fired:
        return 1
    return 0


def main():
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--days", type=int, default=14, help="lookback window in days (default 14)")
    parser.add_argument("--strict", action="store_true", help="exit 1 if any registered hook never fired")
    parser.add_argument("--repo-root", type=Path, default=None,
                        help="explicit checkout root (auto-detected by default)")
    args = parser.parse_args()

    if args.repo_root is not None:
        # Accept explicit path for testing from a worktree.
        telem = args.repo_root / ".claude" / "telemetry" / "hook-fires.jsonl"
        settings = args.repo_root / ".claude" / "settings.json"
    else:
        telem = TELEMETRY
        settings = SETTINGS

    records, total = load_records(telem, args.days)
    registered = extract_hook_names_from_settings(settings)

    print(f"Total records in log: {total}")
    print(f"Records in {args.days}-day window: {len(records)}")
    print(f"Hooks registered in settings.json: {len(registered)}")
    print()

    return report(records, registered, args.days, args.strict)


if __name__ == "__main__":
    sys.exit(main())
