#!/usr/bin/env python3
"""
Standalone test runner for the Workflow launch gate (DESIGN.md §2g, launch
tier): workflow-gate.py's parser and PreToolUse main(). Invokes the hook
directly with synthetic stdin — never spawns a real hook subprocess against
a live session (per DESIGN.md: "test hooks by invoking them directly with
synthetic stdin, not by observing your own session").

Run: python3 .claude/hooks/test_workflow_gate.py
"""
import importlib.util
import io
import json
import sys
import tempfile
from contextlib import redirect_stdout
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
DAEMON_DIR = HOOKS_DIR.parent / "daemon"
sys.path.insert(0, str(DAEMON_DIR))

HOOK_PATH = HOOKS_DIR / "workflow-gate.py"
spec = importlib.util.spec_from_file_location("workflow_gate", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

import valve  # noqa: E402 — same cached module hook.main()'s inner `import valve` sees

# Fake session ids would spawn a real observer subprocess; telemetry would
# pollute the live telemetry.jsonl (the stop-valve tests already had to purge
# 30 such records once). Neutralize both; capture telemetry for assertions.
valve.ensure_observer = lambda *a, **kw: None
TELEMETRY = []
valve.append_telemetry = lambda rec: TELEMETRY.append(rec)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def with_bounce_dir(fn):
    orig = hook.BOUNCE_DIR
    with tempfile.TemporaryDirectory() as td:
        hook.BOUNCE_DIR = td
        try:
            fn(td)
        finally:
            hook.BOUNCE_DIR = orig


def run_hook(payload):
    buf = io.StringIO()
    orig_stdin = sys.stdin
    sys.stdin = io.StringIO(json.dumps(payload))
    try:
        with redirect_stdout(buf):
            hook.main()
    finally:
        sys.stdin = orig_stdin
    return buf.getvalue().strip()


def launch(script=None, session="sess-test", **tool_input):
    if script is not None:
        tool_input["script"] = script
    return run_hook(
        {
            "session_id": session,
            "transcript_path": "/nonexistent/t.jsonl",
            "tool_name": "Workflow",
            "tool_input": tool_input,
        }
    )


def denial_reason(out):
    if not out:
        return None
    return json.loads(out)["hookSpecificOutput"]["permissionDecisionReason"]


GOOD_SCRIPT = """export const meta = {
  name: 'review-changes',
  description: 'Review changed files, 6 agents',
  phases: [{ title: 'Review' }, { title: 'Verify' }],
}
const results = await pipeline(
  DIMENSIONS,
  d => agent(d.prompt, {label: `review:${d.key}`, model: 'sonnet', schema: S}),
  r => parallel(r.findings.map(f => () =>
    agent(`Verify (adversarially): ${f.title}`, {model: 'sonnet', schema: V})))
)
return results
"""

UNMODELED_SCRIPT = """export const meta = { name: 'sweep', description: 'sweep' }
const a = await agent('scan the repo', {schema: S})
const b = await agent('judge it', {model: 'opus'})
return [a, b]
"""

# ---------------------------------------------------------------- parser


def test_parser():
    calls, ok = hook.parse_agent_calls(GOOD_SCRIPT)
    check("parser: balanced script parses", ok)
    check("parser: finds both call sites", len(calls) == 2, str(calls))
    check("parser: all explicit sonnet", all(c["model"] == "sonnet" for c in calls), str(calls))

    calls, ok = hook.parse_agent_calls(UNMODELED_SCRIPT)
    check("parser: inherit detected", [c["model"] for c in calls] == ["inherit", "opus"], str(calls))
    check("parser: line numbers", [c["line"] for c in calls] == [2, 3], str(calls))

    prose = "// spawn an agent (see docs) to help\nconst x = await agent('p', {model: 'haiku'})"
    calls, ok = hook.parse_agent_calls(prose)
    check("parser: 'agent (' prose not a call site", len(calls) == 1 and calls[0]["model"] == "haiku", str(calls))

    in_string = "await agent('write model: sonnet in your report', {schema: S})"
    calls, ok = hook.parse_agent_calls(in_string)
    check("parser: model: inside prompt string ignored", calls[0]["model"] == "inherit", str(calls))

    tmpl = "await agent(`verify ${fn(x)} (really)`, {model: 'sonnet'})"
    calls, ok = hook.parse_agent_calls(tmpl)
    check("parser: parens inside template literal", ok and calls[0]["model"] == "sonnet", str(calls))

    expr = "await agent('p', {model: pick(i)})"
    calls, ok = hook.parse_agent_calls(expr)
    check("parser: non-literal model counts as explicit", calls[0]["model"] == "<expr>", str(calls))

    unbal = "await agent('p', {model: 'sonnet'"
    calls, ok = hook.parse_agent_calls(unbal)
    check("parser: unbalanced reports not-ok", not ok)


# ---------------------------------------------------------------- gate


def test_announce_once(td):
    out1 = launch(GOOD_SCRIPT)
    r1 = denial_reason(out1)
    check("announce: first launch denied", r1 is not None)
    check("announce: reason carries roster", r1 and "2 agent() call sites" in r1, r1 or "")
    check("announce: reason demands model tiers", r1 and "model tier" in r1, r1 or "")
    out2 = launch(GOOD_SCRIPT)
    check("announce: relaunch passes", out2 == "", out2)
    out3 = launch(GOOD_SCRIPT, session="sess-other")
    check("announce: fresh session bounces again", denial_reason(out3) is not None)


def test_model_discipline(td):
    out1 = launch(UNMODELED_SCRIPT)
    r1 = denial_reason(out1)
    check("model: first launch denied with both tiers", r1 and "line 2" in r1 and "announce" in r1.lower(), r1 or "")
    out2 = launch(UNMODELED_SCRIPT)
    r2 = denial_reason(out2)
    check("model: still denied after bounce", r2 is not None)
    check("model: post-bounce deny is model-only", r2 and "Workflow launch paused" not in r2, r2 or "")
    fixed = UNMODELED_SCRIPT.replace("{schema: S}", "{schema: S, model: 'sonnet'}")
    out3 = launch(fixed)
    check("model: fixed script passes (same meta name, new content)", out3 == "", out3)


def test_script_path(td):
    with tempfile.NamedTemporaryFile("w", suffix=".js", delete=False) as f:
        f.write(UNMODELED_SCRIPT)
        path = f.name
    out = launch(scriptPath=path, session="sess-sp")
    r = denial_reason(out)
    check("scriptPath: file is read and checked", r and "line 2" in r, r or "")


def test_saved_and_unparseable(td):
    out1 = launch(name="find-flaky-tests", session="sess-saved")
    r1 = denial_reason(out1)
    check("saved workflow: announce-once still applies", r1 and "enumerate the stages yourself" not in r1)
    out2 = launch(name="find-flaky-tests", session="sess-saved")
    check("saved workflow: second launch passes", out2 == "", out2)

    unbal = "export const meta = { name: 'weird' }\nawait agent('p'"
    out3 = launch(unbal, session="sess-unbal")
    r3 = denial_reason(out3)
    check("unparseable: never model-denied, announce still fires",
          r3 and "not statically parseable" in r3 and "Call sites missing" not in r3, r3 or "")
    out4 = launch(unbal, session="sess-unbal")
    check("unparseable: second launch passes", out4 == "", out4)


def test_fail_open_and_telemetry(td):
    buf = io.StringIO()
    orig = sys.stdin
    sys.stdin = io.StringIO("not json{{{")
    try:
        with redirect_stdout(buf):
            hook.main()
    finally:
        sys.stdin = orig
    check("fail-open: garbage stdin emits nothing", buf.getvalue().strip() == "")

    TELEMETRY.clear()
    launch(GOOD_SCRIPT, session="sess-tel")
    launch(GOOD_SCRIPT, session="sess-tel")
    tiers = [t["tier"] for t in TELEMETRY if t.get("event") == "workflow_gate"]
    check("telemetry: deny then pass recorded", tiers == ["announce-deny", "pass"], str(tiers))
    check("telemetry: workflow identity recorded",
          all(t["workflow"] == "review-changes" for t in TELEMETRY), str(TELEMETRY))


def report():
    for t in (test_announce_once, test_model_discipline, test_script_path,
              test_saved_and_unparseable, test_fail_open_and_telemetry):
        with_bounce_dir(t)
    print(f"PASS {len(PASS)}  FAIL {len(FAIL)}")
    for f in FAIL:
        print("  FAIL:", f)
    sys.exit(1 if FAIL else 0)


if __name__ == "__main__":
    test_parser()
    report()
