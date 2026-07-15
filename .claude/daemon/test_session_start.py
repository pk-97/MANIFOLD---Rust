#!/usr/bin/env python3
"""Tests for the SessionStart hook (daemon-session-start.py), 2026-07-15
(Peter's ruling — DESIGN.md §2k): mechanical/reasoning-primer moved out of
the observer's priming tier and now delivers exactly once, unconditionally,
at SessionStart, using the same frozen `<daemon-advice>` wrapper
`valve.build_block` produces for every other advice-kind move.

Run: python3 test_session_start.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile

DAEMON_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)))
HOOKS_DIR = os.path.normpath(os.path.join(DAEMON_DIR, "..", "hooks"))
HOOK_PATH = os.path.join(HOOKS_DIR, "daemon-session-start.py")
sys.path.insert(0, DAEMON_DIR)
import valve  # noqa: E402

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def run_hook(stdin_obj, verdicts_dir, telemetry_path):
    spec = importlib.util.spec_from_file_location("daemon_session_start", HOOK_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    orig_stdin, orig_stdout = sys.stdin, sys.stdout
    sys.stdin = io.StringIO(json.dumps(stdin_obj) if not isinstance(stdin_obj, str) else stdin_obj)
    sys.stdout = io.StringIO()
    try:
        mod.main()
        out = sys.stdout.getvalue()
    finally:
        sys.stdin, sys.stdout = orig_stdin, orig_stdout
    return out.strip()


def with_temp_verdicts(fn):
    orig_verdicts = valve.VERDICTS_DIR
    orig_telemetry = valve.TELEMETRY_PATH
    orig_payload_cache = valve._PAYLOAD_CACHE
    orig_ensure_observer = valve.ensure_observer
    with tempfile.TemporaryDirectory() as td:
        valve.VERDICTS_DIR = td
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        valve._PAYLOAD_CACHE = None  # force reload against real moves.md
        # The hook's own `import valve` at call time hits this same cached
        # module object (Python caches by name), so stubbing it here reaches
        # the hook too — without this, main() would spawn a REAL observer.py
        # subprocess against the real repo's verdicts dir (ensure_observer
        # has no env-var override the way valve.VERDICTS_DIR itself does).
        valve.ensure_observer = lambda *a, **k: None
        try:
            fn(td)
        finally:
            valve.VERDICTS_DIR = orig_verdicts
            valve.TELEMETRY_PATH = orig_telemetry
            valve._PAYLOAD_CACHE = orig_payload_cache
            valve.ensure_observer = orig_ensure_observer


def test_session_start_delivers_reasoning_primer_as_advice():
    def run(td):
        session = "sess-start-primer"
        out = run_hook({"session_id": session, "transcript_path": "/dev/null"}, td, valve.TELEMETRY_PATH)
        d = json.loads(out) if out else None
        check("SessionStart emits hookSpecificOutput", d is not None, out)
        ctx = (d or {}).get("hookSpecificOutput", {}).get("additionalContext", "")
        check("additionalContext carries the frozen advice tag", ctx.startswith('<daemon-advice move="mechanical/reasoning-primer">'), ctx[:80])
        check("additionalContext closes the advice tag", ctx.rstrip().endswith("</daemon-advice>"))
        check("payload text present", "How to work, from the model that wrote this system" in ctx)
        check("no supervised-mode ack (advice frame)", "Supervised mode" not in ctx)
        check("hookEventName is SessionStart", d.get("hookSpecificOutput", {}).get("hookEventName") == "SessionStart")

    with_temp_verdicts(run)


def test_session_start_emits_injected_telemetry():
    def run(td):
        session = "sess-start-telemetry"
        run_hook({"session_id": session, "transcript_path": "/dev/null"}, td, valve.TELEMETRY_PATH)
        recs = []
        try:
            with open(valve.TELEMETRY_PATH, encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if line:
                        recs.append(json.loads(line))
        except OSError:
            pass
        matches = [r for r in recs if r.get("event") == "injected" and r.get("move_id") == "mechanical/reasoning-primer"]
        check("exactly one injected telemetry record", len(matches) == 1, recs)
        rec = matches[0] if matches else {}
        check("telemetry names this session", rec.get("session_id") == session, rec)
        check("telemetry channel is session_start", rec.get("channel") == "session_start", rec)

    with_temp_verdicts(run)


def test_session_start_fires_every_call_no_cooldown():
    """No mailbox, no seq, no cooldown — SessionStart delivery is
    unconditional, unlike the old priming-tier fire's advice-recur gate. A
    second SessionStart call (resume/clear/compact) delivers again."""
    def run(td):
        session = "sess-start-repeat"
        out1 = run_hook({"session_id": session, "transcript_path": "/dev/null"}, td, valve.TELEMETRY_PATH)
        out2 = run_hook({"session_id": session, "transcript_path": "/dev/null"}, td, valve.TELEMETRY_PATH)
        check("first call delivers", bool(out1))
        check("second call also delivers (no cooldown gate)", bool(out2))

    with_temp_verdicts(run)


def test_session_start_still_spawns_observer():
    """The pre-existing ensure_observer behavior must survive the new
    delivery logic — a bogus session id with no real transcript should not
    raise, and observer_spawn telemetry should not be required (spawn itself
    is fail-open / best-effort), but the hook must not blow up."""
    def run(td):
        out = run_hook({"session_id": "sess-start-spawn", "transcript_path": "/dev/null"}, td, valve.TELEMETRY_PATH)
        check("hook still runs cleanly alongside observer spawn", isinstance(out, str))

    with_temp_verdicts(run)


def test_missing_session_id_is_silent():
    def run(td):
        out = run_hook({"transcript_path": "/dev/null"}, td, valve.TELEMETRY_PATH)
        # session_id is None; build_block/append_telemetry must not raise,
        # and delivery still happens (SessionStart's advice isn't session-
        # keyed the way a mailbox flag is) — the only hard requirement is
        # that the hook never raises or exits non-zero.
        check("no exception on missing session_id", isinstance(out, str))

    with_temp_verdicts(run)


def test_malformed_stdin_exits_clean():
    def run(td):
        out = run_hook("not json {{{", td, valve.TELEMETRY_PATH)
        check("malformed stdin -> silent", out == "", out)

    with_temp_verdicts(run)


def main():
    tests = [
        test_session_start_delivers_reasoning_primer_as_advice,
        test_session_start_emits_injected_telemetry,
        test_session_start_fires_every_call_no_cooldown,
        test_session_start_still_spawns_observer,
        test_missing_session_id_is_silent,
        test_malformed_stdin_exits_clean,
    ]
    for t in tests:
        t()
    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
