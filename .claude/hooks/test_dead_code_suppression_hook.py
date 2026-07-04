#!/usr/bin/env python3
"""
Standalone test runner for dead-code-suppression-hook.py. Invokes the hook's
functions directly with synthetic tool_input dicts — never spawns a real
subprocess or touches actual repo files.

Run: python3 .claude/hooks/test_dead_code_suppression_hook.py
"""
import importlib.util
import io
import json
import sys
from contextlib import redirect_stdout
from pathlib import Path

HOOK_PATH = Path(__file__).resolve().parent / "dead-code-suppression-hook.py"

spec = importlib.util.spec_from_file_location("dead_code_suppression_hook", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

PASS = []
FAIL = []


def check(name, cond, detail=""):
    if cond:
        PASS.append(name)
    else:
        FAIL.append((name, detail))


def run_hook(payload: dict):
    """Feed `payload` to hook.main() via stdin/stdout patching. Returns the
    parsed additionalContext string, or None if the hook stayed silent."""
    stdin = io.StringIO(json.dumps(payload))
    stdout = io.StringIO()
    orig_stdin, orig_stdout = sys.stdin, sys.stdout
    sys.stdin = stdin
    try:
        with redirect_stdout(stdout):
            hook.main()
    finally:
        sys.stdin = orig_stdin
        sys.stdout = orig_stdout
    out = stdout.getvalue().strip()
    if not out:
        return None
    obj = json.loads(out)
    return obj["hookSpecificOutput"]["additionalContext"]


def test_added_bare_fires():
    payload = {
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "old_string": "fn helper() {}",
            "new_string": "#[allow(dead_code)]\nfn helper() {}",
        },
    }
    ctx = run_hook(payload)
    check("added bare marker -> fires", ctx is not None, ctx)


def test_added_with_reason_silent():
    payload = {
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "old_string": "fn helper() {}",
            "new_string": "// un-suppress when the session-P4 grid UI wires this in\n#[allow(dead_code)]\nfn helper() {}",
        },
    }
    ctx = run_hook(payload)
    check("added marker with reason comment above -> silent", ctx is None, ctx)


def test_reason_on_same_line_silent():
    payload = {
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "old_string": "fn helper() {}",
            "new_string": "#[allow(dead_code)] // waiting on session P4\nfn helper() {}",
        },
    }
    ctx = run_hook(payload)
    check("added marker with trailing reason on same line -> silent", ctx is None, ctx)


def test_preexisting_marker_in_old_string_silent():
    payload = {
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "old_string": "#[allow(dead_code)]\nfn helper() {}",
            "new_string": "#[allow(dead_code)]\nfn helper() { do_more(); }",
        },
    }
    ctx = run_hook(payload)
    check("marker already present in old_string -> silent", ctx is None, ctx)


def test_non_rust_file_silent():
    payload = {
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "docs/NOTES.md",
            "old_string": "plain text",
            "new_string": "#[allow(dead_code)]\nfn helper() {}",
        },
    }
    ctx = run_hook(payload)
    check("non-Rust file -> silent", ctx is None, ctx)


def test_write_with_marker_fires():
    payload = {
        "tool_name": "Write",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "content": "mod x;\n#[allow(dead_code)]\nfn helper() {}\n",
        },
    }
    ctx = run_hook(payload)
    check("Write introducing bare marker -> fires", ctx is not None, ctx)


def test_write_with_annotated_marker_silent():
    payload = {
        "tool_name": "Write",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "content": "mod x;\n// waiting on session P4 grid UI\n#[allow(dead_code)]\nfn helper() {}\n",
        },
    }
    ctx = run_hook(payload)
    check("Write with annotated marker -> silent", ctx is None, ctx)


def test_multiedit_added_bare_fires():
    payload = {
        "tool_name": "MultiEdit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "edits": [
                {"old_string": "fn a() {}", "new_string": "fn a() {}"},
                {
                    "old_string": "fn helper() {}",
                    "new_string": "#[allow(unused_variables)]\nfn helper() {}",
                },
            ],
        },
    }
    ctx = run_hook(payload)
    check("MultiEdit added bare marker in one edit -> fires", ctx is not None, ctx)


def test_multiedit_marker_moved_within_call_silent():
    """Marker present in one edit's old_string and again in another edit's
    new_string within the SAME call reads as pre-existing (aggregated check)."""
    payload = {
        "tool_name": "MultiEdit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "edits": [
                {
                    "old_string": "#[allow(dead_code)]\nfn old_helper() {}",
                    "new_string": "",
                },
                {
                    "old_string": "fn new_helper() {}",
                    "new_string": "#[allow(dead_code)]\nfn new_helper() {}",
                },
            ],
        },
    }
    ctx = run_hook(payload)
    check("marker moved across edits within one call -> silent", ctx is None, ctx)


def test_no_marker_at_all_silent():
    payload = {
        "tool_name": "Edit",
        "tool_input": {
            "file_path": "crates/foo/src/lib.rs",
            "old_string": "fn a() {}",
            "new_string": "fn a() { b(); }",
        },
    }
    ctx = run_hook(payload)
    check("no marker involved -> silent", ctx is None, ctx)


def main():
    test_added_bare_fires()
    test_added_with_reason_silent()
    test_reason_on_same_line_silent()
    test_preexisting_marker_in_old_string_silent()
    test_non_rust_file_silent()
    test_write_with_marker_fires()
    test_write_with_annotated_marker_silent()
    test_multiedit_added_bare_fires()
    test_multiedit_marker_moved_within_call_silent()
    test_no_marker_at_all_silent()

    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
