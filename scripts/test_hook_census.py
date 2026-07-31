#!/usr/bin/env python3
"""Self-test for hook_census.py — reads synthetic telemetry JSONL.

Covers:

  1. decision derivation   — permissionDecision, block, context, missing
  2. acted detection       — out>0 and exit!=0 both count; silent records don't
  3. never-fired detection — registered hooks absent from the log window
  4. settings parsing      — hook names extracted from settings.json commands
  5. --strict exit code    — exit 1 when never-fired hooks exist under --strict
  6. malformed lines       — JSON parse errors and garbage lines are tolerated
  7. ts parsing            — ISO-8601 with trailing Z and +HH:MM offsets

Run: scripts/test_hook_census.py
"""

import json
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import hook_census  # noqa: E402

FAILURES = []


def check(label, got, want):
    ok = got == want
    print(f"  {'ok  ' if ok else 'FAIL'} {label}: got {got!r} want {want!r}")
    if not ok:
        FAILURES.append(label)


def check_true(label, cond):
    print(f"  {'ok  ' if cond else 'FAIL'} {label}")
    if not cond:
        FAILURES.append(label)


def case_decision_aggregation(tmp):
    """per_hook_stats counts deny/ask/block as blocks, context and absent as not."""
    print("1. decision aggregation")
    records = [
        {"hook": "guard.py", "out": 100, "exit": 0, "decision": "deny",
         "ts": "2026-07-30T12:00:00Z"},
        {"hook": "guard.py", "out": 0, "exit": 0, "decision": "ask",
         "ts": "2026-07-30T12:00:01Z"},
        {"hook": "guard.py", "out": 0, "exit": 0, "decision": "block",
         "ts": "2026-07-30T12:00:02Z"},
        {"hook": "nudge.py", "out": 50, "exit": 0, "decision": "context",
         "ts": "2026-07-30T12:00:00Z"},
        {"hook": "nudge.py", "out": 0, "exit": 0,
         "ts": "2026-07-30T12:00:01Z"},
        {"hook": "nudge.py", "out": 0, "exit": 0,
         "ts": "2026-07-30T12:00:02Z"},
    ]
    stats = hook_census.per_hook_stats(records)
    stats_map = {s[0]: s for s in stats}
    guard = stats_map["guard.py"]
    check("guard fires", guard[1], 3)
    check("guard acted (out>0)", guard[2], 1)  # first record has out>0
    check("guard blocks", guard[3], 3)
    nudge = stats_map["nudge.py"]
    check("nudge fires", nudge[1], 3)
    check("nudge blocks (context not counted)", nudge[3], 0)


def case_acted_detection(tmp):
    """Records with out>0 or exit!=0 count as acted."""
    print("2. acted detection")
    records = [
        {"hook": "a.py", "out": 0, "exit": 0, "ts": "2026-07-30T12:00:00Z"},
        {"hook": "a.py", "out": 1, "exit": 0, "ts": "2026-07-30T12:00:01Z"},
        {"hook": "a.py", "out": 0, "exit": 1, "ts": "2026-07-30T12:00:02Z"},
        {"hook": "a.py", "out": 5, "exit": 2, "ts": "2026-07-30T12:00:03Z"},
    ]
    stats = hook_census.per_hook_stats(records)
    a = stats[0]
    check("a fires", a[1], 4)
    check("a acted (3 of 4)", a[2], 3)
    check("a blocks (no decision)", a[3], 0)


def case_never_fired(tmp):
    """report() lists hooks registered but absent from the window."""
    print("3. never-fired detection")
    records = [
        {"hook": "fired.py", "out": 0, "exit": 0, "ts": "2026-07-30T12:00:00Z"},
    ]
    registered = {"fired.py", "never.py", "also-never.py"}
    ec = hook_census.report(records, registered, 14, strict=False)
    check("non-strict exit", ec, 0)

    ec_strict = hook_census.report(records, registered, 14, strict=True)
    check("strict exit with never-fired", ec_strict, 1)


def case_no_never_fired_strict(tmp):
    """When all registered hooks appear, --strict exits 0."""
    print("4. strict exit 0 when all registered hooks fired")
    records = [
        {"hook": "a.py", "out": 0, "exit": 0, "ts": "2026-07-30T12:00:00Z"},
        {"hook": "b.py", "out": 0, "exit": 0, "ts": "2026-07-30T12:00:00Z"},
    ]
    registered = {"a.py", "b.py"}
    ec = hook_census.report(records, registered, 14, strict=True)
    check("strict exit when no never-fired", ec, 0)


def case_never_acted_detection(tmp):
    """report() lists hooks that fired but never acted."""
    print("5. never-acted detection")
    records = [
        {"hook": "acting.py", "out": 10, "exit": 0,
         "ts": "2026-07-30T12:00:00Z"},
        {"hook": "acting.py", "out": 0, "exit": 0,
         "ts": "2026-07-30T12:00:01Z"},
        {"hook": "silent.py", "out": 0, "exit": 0,
         "ts": "2026-07-30T12:00:00Z"},
    ]
    # capture stdout to check the never-acted list
    import io
    buf = io.StringIO()
    old = sys.stdout
    sys.stdout = buf
    try:
        hook_census.report(records, {"acting.py", "silent.py"}, 14, strict=False)
    finally:
        sys.stdout = old
    output = buf.getvalue()
    check_true("never-acted list mentions silent.py", "silent.py" in output)
    # acting.py IS in the activity table (it fired), but NOT in the NEVER ACTED
    # section.  Check only whether it appears after "NEVER ACTED".
    after_never_acted = output.split("NEVER ACTED")[-1] if "NEVER ACTED" in output else output
    check_true("never-acted does not mention acting.py", "acting.py" not in after_never_acted)


def case_settings_parsing(tmp):
    """extract_hook_names_from_settings finds all wrapped hooks."""
    print("6. settings.json parsing")
    settings = {
        "hooks": {
            "PreToolUse": [
                {"hooks": [
                    {"command": "python3 \"$PROJECT/.claude/hooks/hook_telemetry.py\" preToolUseBash.py"},
                    {"command": "python3 \"$PROJECT/.claude/hooks/hook_telemetry.py\" guard.py"},
                ]},
                {"hooks": [
                    {"command": "python3 \"$PROJECT/.claude/hooks/hook_telemetry.py\" nudge.py"},
                ]},
            ],
            "SessionStart": [
                {"hooks": [
                    {"command": "python3 \"$PROJECT/.claude/hooks/hook_telemetry.py\" identity.py"},
                ]},
            ],
            "SessionEnd": [
                {"hooks": [
                    {"command": "python3 \"$PROJECT/.claude/hooks/hook_telemetry.py\" scrub.py \"/tmp/foo\""},
                ]},
            ],
        }
    }
    path = tmp / "settings.json"
    with open(path, "w") as f:
        json.dump(settings, f)
    names = hook_census.extract_hook_names_from_settings(path)
    expected = {"preToolUseBash.py", "guard.py", "nudge.py", "identity.py", "scrub.py"}
    check("extracted hook names", names, expected)


def case_extract_hook_name():
    """_extract_hook_name handles various command patterns."""
    print("7. hook name extraction")
    cases = [
        ('python3 "...hook_telemetry.py" preToolUseBash.py', "preToolUseBash.py"),
        ('python3 "...hook_telemetry.py" guard.py "/tmp/x"', "guard.py"),
        ('python3 direct.py', "direct.py"),
        ('python3 "..."', None),
        ("", None),
    ]
    for cmd, want in cases:
        got = hook_census._extract_hook_name(cmd)
        check(f"extract from {cmd!r}", got, want)


def case_malformed_log_lines(tmp):
    """load_records skips lines that are not valid JSON."""
    print("8. malformed log line tolerance")
    path = tmp / "hook-fires.jsonl"
    with open(path, "w") as f:
        f.write('{"hook": "good.py", "out": 0, "exit": 0, "ts": "2026-07-30T12:00:00Z"}\n')
        f.write("not valid json\n")
        f.write('{"hook": "also-good.py", "out": 0, "exit": 0, "ts": "2026-07-30T12:00:00Z"}\n')
        f.write("\n")
        f.write('garbage\n')
    records, total = hook_census.load_records(path, 14)
    check("total lines counted (empty line skipped)", total, 4)
    check("parseable records", len(records), 2)


def case_date_filter(tmp):
    """load_records respects the --days window."""
    print("9. date filtering")
    path = tmp / "hook-fires.jsonl"
    # Write records at known timestamps
    now = datetime.now(timezone.utc)
    old = now.isoformat(timespec="seconds").replace("+00:00", "Z")  # old-style
    fresh = now.isoformat(timespec="seconds").replace("+00:00", "Z")
    with open(path, "w") as f:
        f.write(json.dumps({"hook": "old.py", "out": 0, "exit": 0, "ts": old}) + "\n")
        f.write(json.dumps({"hook": "fresh.py", "out": 0, "exit": 0, "ts": fresh}) + "\n")
    # 1-day window should include both if they're recent
    records, total = hook_census.load_records(path, 1)
    check("1-day window count", len(records), 2)
    # 0-day window should exclude everything (cutoff is now, no record is >= now)
    records0, _ = hook_census.load_records(path, 0)
    check("0-day window count (expect 0)", len(records0), 0)


def case_ts_parsing():
    """_parse_ts handles ISO-8601 variants."""
    print("10. timestamp parsing")
    cases = [
        ("2026-07-30T12:00:00Z", True),
        ("2026-07-30T12:00:00+00:00", True),
        ("2026-07-30T12:00:00", True),
        ("not-a-date", None),
        ("", None),
    ]
    for raw, should_be_valid in cases:
        result = hook_census._parse_ts(raw)
        if should_be_valid is True:
            check_true(f"parse {raw!r} succeeds", result is not None)
            check_true(f"parse {raw!r} has tzinfo", result.tzinfo is not None if "Z" in raw or "+" in raw else True)
        else:
            check(f"parse {raw!r} returns None", result, should_be_valid)


def main():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        case_decision_aggregation(tmp)
        case_acted_detection(tmp)
        case_never_fired(tmp)
        case_no_never_fired_strict(tmp)
        case_never_acted_detection(tmp)
        case_settings_parsing(tmp)
        case_malformed_log_lines(tmp)
        case_date_filter(tmp)
    case_extract_hook_name()
    case_ts_parsing()
    if FAILURES:
        print(f"\nFAILED: {len(FAILURES)} check(s): {', '.join(FAILURES)}")
        return 1
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
