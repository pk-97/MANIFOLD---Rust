#!/usr/bin/env python3
"""
Standalone test for the Stop-hook valve (daemon-stop.py), DESIGN.md §2
"Known delivery gap" + moves.md mechanical/announced-not-started.

Runs the real hook as a subprocess against synthetic stdin and planted
verdict/transcript files under a temp VERDICTS_DIR (monkeypatched into
`valve` for the in-process helper calls, and passed via env for the
subprocess — see `run_hook`). Never touches the real verdicts dir.

Run: python3 .claude/daemon/test_stop_valve.py
"""
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
HOOK_PATH = DAEMON_DIR.parent / "hooks" / "daemon-stop.py"
sys.path.insert(0, str(DAEMON_DIR))
import valve  # noqa: E402

# A second, persistently-loaded copy of the hook module for testing the
# grade-backstop helper functions directly (2026-07-05 addressability fix) —
# `run_hook` below deliberately reloads a FRESH module per call (see its own
# docstring), which is right for full end-to-end runs but leaves no handle
# to call a helper in isolation. This is a separate module object; nothing
# it does affects run_hook's own fresh-load behavior.
_direct_spec = importlib.util.spec_from_file_location("daemon_stop_direct", HOOK_PATH)
DIRECT = importlib.util.module_from_spec(_direct_spec)
_direct_spec.loader.exec_module(DIRECT)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def run_hook(stdin_obj, verdicts_dir):
    """Run the real daemon-stop.py as a subprocess with VERDICTS_DIR
    redirected via monkeypatch shim: the hook imports valve fresh in its own
    process, so we point it at our temp dir by writing a tiny sitecustomize-
    style override isn't available — instead we patch valve.py's module
    globals via an env-var-driven shim would require editing valve.py, which
    this test must not do. Simplest correct approach: run the hook in-process
    by exec'ing its main() with sys.path already pointed at DAEMON_DIR, after
    monkeypatching `valve.VERDICTS_DIR`/`valve.WORKER_NUDGES_FLAG` for the
    imported valve module the hook itself will import (same module object,
    since Python caches by sys.path + name)."""
    import importlib.util

    spec = importlib.util.spec_from_file_location("daemon_stop", HOOK_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    orig_stdin = sys.stdin
    orig_stdout = sys.stdout
    import io

    sys.stdin = io.StringIO(json.dumps(stdin_obj) if not isinstance(stdin_obj, str) else stdin_obj)
    sys.stdout = io.StringIO()
    try:
        mod.main()
        out = sys.stdout.getvalue()
    finally:
        sys.stdin = orig_stdin
        sys.stdout = orig_stdout
    return out.strip()


def with_temp_verdicts(fn):
    orig_verdicts = valve.VERDICTS_DIR
    orig_flag = valve.WORKER_NUDGES_FLAG
    orig_telemetry = valve.TELEMETRY_PATH
    orig_eval_dir = valve.EVAL_DIR
    orig_payload_cache = valve._PAYLOAD_CACHE
    with tempfile.TemporaryDirectory() as td:
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        # Sleep pass 1: tests were writing `injected` records into the LIVE
        # telemetry.jsonl (30 sess-* records polluted the graded week).
        valve.TELEMETRY_PATH = os.path.join(td, "telemetry.jsonl")
        # Grade-backstop tests (2026-07-05) need their own eval/ dir — never
        # the real .claude/daemon/eval/live_grades*.jsonl.
        valve.EVAL_DIR = os.path.join(td, "eval")
        valve._PAYLOAD_CACHE = None  # force reload against real moves.md (unpatched path)
        try:
            fn(td)
        finally:
            valve.VERDICTS_DIR = orig_verdicts
            valve.WORKER_NUDGES_FLAG = orig_flag
            valve.TELEMETRY_PATH = orig_telemetry
            valve.EVAL_DIR = orig_eval_dir
            valve._PAYLOAD_CACHE = orig_payload_cache


def write_telemetry_records(records):
    os.makedirs(os.path.dirname(valve.TELEMETRY_PATH), exist_ok=True)
    with open(valve.TELEMETRY_PATH, "a", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")


def write_grade_records(records, filename="live_grades.session.jsonl"):
    os.makedirs(valve.EVAL_DIR, exist_ok=True)
    with open(os.path.join(valve.EVAL_DIR, filename), "a", encoding="utf-8") as f:
        for r in records:
            f.write(json.dumps(r) + "\n")


def write_transcript_with_events(path, n_events, start_ts, final_text="Here is the summary of what I found."):
    """A transcript with `n_events` tool_result-bearing user records at
    1-second intervals starting just after `start_ts`, followed by a final
    assistant text that is NOT an announcement (so _announced_not_started
    stays quiet and the grade-backstop check is reached)."""

    def iso(ts):
        return datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    lines = []
    for i in range(n_events):
        lines.append(
            json.dumps(
                {
                    "type": "user",
                    "timestamp": iso(start_ts + i + 1),
                    "message": {
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": f"t{i}", "content": "ok"}],
                    },
                }
            )
        )
    lines.append(
        json.dumps(
            {
                "type": "assistant",
                "timestamp": iso(start_ts + n_events + 1),
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": final_text}]},
            }
        )
    )
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def write_verdict(verdicts_dir, key, move_id, seq, ts=None):
    path = os.path.join(verdicts_dir, f"{key}.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(
            {
                "ts": ts if ts is not None else time.time(),
                "flag": {"move_id": move_id, "seq": seq, "evidence": "test", "confidence": 0.9},
            },
            f,
        )


def write_chat_claim_transcript(path, final_text, user_prompt_text="continue with the plan", prior_tool_calls=None):
    """A transcript ending in one zero-tool-call turn — mechanical/ungrounded-
    chat-claim's target shape. `prior_tool_calls` is a list of (name, input_)
    or (name, input_, result_text) items rendered as an earlier, already-
    completed turn: each becomes an assistant tool_use + a matching user
    tool_result, giving the grounding scan something to find in the CALL'S
    INPUT (or, for the un-grounded tests, deliberately nothing) — and
    `result_text` lets a test put an artifact in the tool's OUTPUT instead,
    to prove output mentions never ground anything. The real turn under test
    is a genuine user prompt followed by one assistant message containing
    ONLY a text block — no tool_use at all."""
    lines = []
    ts = 0

    def next_ts():
        nonlocal ts
        ts += 1
        return f"2026-07-04T00:{ts // 60:02d}:{ts % 60:02d}Z"

    for i, spec in enumerate(prior_tool_calls or []):
        name, input_ = spec[0], spec[1]
        result_text = spec[2] if len(spec) > 2 else "some tool output text"
        lines.append(json.dumps({
            "type": "assistant",
            "timestamp": next_ts(),
            "message": {
                "role": "assistant",
                "model": "claude-sonnet-5",
                "content": [{"type": "tool_use", "id": f"tu{i}", "name": name, "input": input_}],
            },
        }))
        lines.append(json.dumps({
            "type": "user",
            "timestamp": next_ts(),
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": f"tu{i}", "content": result_text}],
            },
        }))
    lines.append(json.dumps({
        "type": "user",
        "timestamp": next_ts(),
        "message": {"role": "user", "content": user_prompt_text},
    }))
    lines.append(json.dumps({
        "type": "assistant",
        "timestamp": next_ts(),
        "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": final_text}]},
    }))
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def write_done_claim_transcript(path, final_text, turn_tool_calls, user_prompt_text="please do the thing"):
    """A transcript whose LAST turn contains tool calls (assistant tool_use +
    user tool_result pairs AFTER the human prompt, i.e. inside the turn) and
    ends on a final assistant text — mechanical/unverified-done-claim's
    target shape. `turn_tool_calls` is a list of (name, input_dict)."""
    lines = [json.dumps({
        "type": "user",
        "timestamp": "2026-07-04T00:00:00Z",
        "message": {"role": "user", "content": user_prompt_text},
    })]
    ts = 0
    for i, (name, input_) in enumerate(turn_tool_calls):
        ts += 1
        lines.append(json.dumps({
            "type": "assistant",
            "timestamp": f"2026-07-04T00:00:{ts:02d}Z",
            "message": {
                "role": "assistant",
                "model": "claude-sonnet-5",
                "content": [{"type": "tool_use", "id": f"dt{i}", "name": name, "input": input_}],
            },
        }))
        ts += 1
        lines.append(json.dumps({
            "type": "user",
            "timestamp": f"2026-07-04T00:00:{ts:02d}Z",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": f"dt{i}", "content": "ok"}]},
        }))
    lines.append(json.dumps({
        "type": "assistant",
        "timestamp": f"2026-07-04T00:00:{ts + 1:02d}Z",
        "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": final_text}]},
    }))
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def write_transcript(path, final_text, tool_use_after_text=False):
    """A minimal transcript whose LAST line is a single assistant message.
    If tool_use_after_text, a tool_use block follows the text block within
    that same message (so the mechanical check must NOT fire)."""
    content = [{"type": "text", "text": final_text}]
    if tool_use_after_text:
        content.append({"type": "tool_use", "id": "tu1", "name": "Read", "input": {"file_path": "x"}})
    with open(path, "w", encoding="utf-8") as f:
        f.write(json.dumps({"type": "user", "message": {"role": "user", "content": "do the thing"}, "timestamp": "2026-07-04T00:00:00Z"}) + "\n")
        f.write(json.dumps({
            "type": "assistant",
            "message": {"role": "assistant", "model": "claude-sonnet-5", "content": content},
            "timestamp": "2026-07-04T00:00:01Z",
        }) + "\n")


def test_pending_flag_blocks_once_and_sentinel_guards_repeat():
    def run(td):
        session = "sess-pending"
        write_verdict(td, session, "anchor/verify-claim", seq=1)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        d1 = json.loads(out1) if out1 else None
        check("first call blocks", d1 is not None and d1.get("decision") == "block", out1)
        check("reason carries the move tag", d1 and 'move="anchor/verify-claim"' in d1.get("reason", ""), d1)
        check("sentinel file written", os.path.exists(os.path.join(td, f"{session}.stopblock.p1")))
        check("consumed marker written", valve.read_consumed(session) == 1)

        # A fresh, higher-seq flag lands (as if the observer fired again),
        # but the SAME prompt_id already spent its block this turn.
        write_verdict(td, session, "anchor/verify-claim", seq=2)
        out2 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        check("second call for same prompt_id is silent (sentinel guard)", out2 == "", out2)
    with_temp_verdicts(run)


def test_stop_hook_active_short_circuits():
    def run(td):
        session = "sess-active"
        write_verdict(td, session, "anchor/verify-claim", seq=1)
        out = run_hook({"session_id": session, "prompt_id": "p1", "stop_hook_active": True, "transcript_path": "/dev/null"}, td)
        check("stop_hook_active suppresses block even with pending flag", out == "", out)
        check("no sentinel written when stop_hook_active short-circuits", not os.path.exists(os.path.join(td, f"{session}.stopblock.p1")))
    with_temp_verdicts(run)


def test_announcement_ending_blocks_with_mechanical_payload():
    def run(td):
        session = "sess-announce"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "I found the issue. Starting the migration now with the new schema.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check("announcement ending blocks", d is not None and d.get("decision") == "block", out)
        check("reason carries mechanical move id", d and 'move="mechanical/announced-not-started"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_beginning_and_let_me_now_variants_fire():
    def run(td):
        for i, text in enumerate(["Beginning the refactor across the crate.", "Let me now build the harness."]):
            session = f"sess-variant-{i}"
            transcript = os.path.join(td, f"t{i}.jsonl")
            write_transcript(transcript, text)
            out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
            d = json.loads(out) if out else None
            check(f"variant fires: {text!r}", d is not None and d.get("decision") == "block", out)
    with_temp_verdicts(run)


def test_tool_use_after_text_does_not_fire():
    def run(td):
        session = "sess-tooluse-after"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Starting the migration now.", tool_use_after_text=True)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("tool_use after the text suppresses the mechanical fire", out == "", out)
    with_temp_verdicts(run)


def test_handoff_and_question_endings_do_not_fire():
    def run(td):
        cases = [
            "I'll do this once you confirm the approach.",
            "Should I start the migration now?",
            "Next session I'll pick this up.",
        ]
        for i, text in enumerate(cases):
            session = f"sess-noannounce-{i}"
            transcript = os.path.join(td, f"t{i}.jsonl")
            write_transcript(transcript, text)
            out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
            check(f"non-announcement stays silent: {text!r}", out == "", out)
    with_temp_verdicts(run)


# ---- mechanical/ungrounded-chat-claim (DESIGN.md §2h.1) ----


def test_chat_claim_fires_on_ungrounded_artifact():
    def run(td):
        session = "sess-chatclaim-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "The fix lives in docs/TOTALLY_UNSEEN_FILE.md and should be straightforward.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check("ungrounded artifact in a zero-tool-call turn fires", d is not None and d.get("decision") == "block", out)
        check("reason carries the chat-claim move id", d and 'move="mechanical/ungrounded-chat-claim"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_chat_claim_silent_when_grounded_by_earlier_tool_input():
    def run(td):
        session = "sess-chatclaim-grounded"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "The fix lives in docs/TOTALLY_UNSEEN_FILE.md and should be straightforward.",
            prior_tool_calls=[("Read", {"file_path": "docs/TOTALLY_UNSEEN_FILE.md"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("an earlier tool call's INPUT grounds the artifact — stays silent", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_tool_output_never_grounds():
    def run(td):
        session = "sess-chatclaim-output-only"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "The fix lives in docs/TOTALLY_UNSEEN_FILE.md and should be straightforward.",
            prior_tool_calls=[
                ("Bash", {"command": "cargo test --workspace"}, "note: docs/TOTALLY_UNSEEN_FILE.md was referenced in the log"),
            ],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "a mention inside a tool's OUTPUT does not ground it — still fires",
            d is not None and d.get("decision") == "block" and 'move="mechanical/ungrounded-chat-claim"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_chat_claim_silent_for_fenced_artifact():
    def run(td):
        session = "sess-chatclaim-fenced"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "```\ndocs/TOTALLY_UNSEEN_FILE.md\n```\nEverything here looks fine.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("an artifact only inside a fenced block is not extracted", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_silent_when_user_echoed_artifact_this_turn():
    def run(td):
        session = "sess-chatclaim-echo"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "docs/TOTALLY_UNSEEN_FILE.md looks fine to me.",
            user_prompt_text="can you check docs/TOTALLY_UNSEEN_FILE.md",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("an artifact the user's own message introduced this turn is excluded", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_silent_with_recall_marker():
    def run(td):
        session = "sess-chatclaim-recall"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "I think the fix is in docs/TOTALLY_UNSEEN_FILE.md.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a self-marked recall/proposal skips the whole detection", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_silent_when_turn_has_any_tool_call():
    def run(td):
        # A tool call happens EARLIER in the turn (not the final message) —
        # moves.md's "turn contains zero tool calls" is turn-wide, not just a
        # check on the last assistant message's own trailing content (that
        # narrower question is mechanical/announced-not-started's, not this
        # move's).
        session = "sess-chatclaim-turnwide"
        transcript = os.path.join(td, "t.jsonl")
        lines = [
            json.dumps({"type": "user", "timestamp": "2026-07-04T00:00:00Z", "message": {"role": "user", "content": "please fix this"}}),
            json.dumps({
                "type": "assistant",
                "timestamp": "2026-07-04T00:00:01Z",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "tu0", "name": "Read", "input": {"file_path": "src/lib.rs"}},
                ]},
            }),
            json.dumps({
                "type": "user",
                "timestamp": "2026-07-04T00:00:02Z",
                "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "tu0", "content": "ok"}]},
            }),
            json.dumps({
                "type": "assistant",
                "timestamp": "2026-07-04T00:00:03Z",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "text", "text": "The fix lives in docs/TOTALLY_UNSEEN_FILE.md and should be straightforward."},
                ]},
            }),
        ]
        with open(transcript, "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a tool call anywhere in the turn (not just after the final text) suppresses the fire", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_allcaps_token_resolves_against_real_docs():
    def run(td):
        # Uses a real doc in THIS repo (docs/DESIGN_DOC_STANDARD.md) so the
        # end-to-end hook run exercises the real REPO_ROOT derivation, not a
        # monkeypatched one.
        session = "sess-chatclaim-allcaps-real"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "Everything about DESIGN_DOC_STANDARD is already implemented, no changes needed.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "an ALL-CAPS token resolving to a real docs/<token>.md fires when ungrounded",
            d is not None and d.get("decision") == "block" and 'move="mechanical/ungrounded-chat-claim"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_chat_claim_allcaps_token_that_does_not_resolve_is_not_an_artifact():
    def run(td):
        session = "sess-chatclaim-allcaps-fake"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "TOTALLY_MADE_UP_TOKEN_XYZ handles this correctly already.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("an ALL-CAPS token with no matching docs/<token>.md is never a candidate", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_loses_priority_to_announced_not_started():
    def run(td):
        session = "sess-chatclaim-priority-announce"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "The bug lives in docs/TOTALLY_UNSEEN_FILE.md. Starting the fix now.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "announced-not-started wins when a turn matches both signatures",
            d is not None and 'move="mechanical/announced-not-started"' in d.get("reason", ""),
            out,
        )
        check("chat-claim did not also fire in the same block", d and 'move="mechanical/ungrounded-chat-claim"' not in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_chat_claim_loses_priority_to_pending_whisper():
    def run(td):
        session = "sess-chatclaim-priority-pending"
        transcript = os.path.join(td, "t.jsonl")
        write_verdict(td, session, "anchor/verify-claim", seq=1)
        write_chat_claim_transcript(transcript, "The fix lives in docs/TOTALLY_UNSEEN_FILE.md.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "an already-pending whisper wins over the chat-claim check",
            d is not None and 'move="anchor/verify-claim"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_chat_claim_applies_to_worker_when_flag_enabled():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-chatclaim-worker", "wk9"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(transcript, "The fix lives in docs/TOTALLY_UNSEEN_FILE.md.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "chat-claim reaches a worker Stop event when the worker-nudges flag is on",
            d is not None and 'move="mechanical/ungrounded-chat-claim"' in d.get("reason", ""),
            out,
        )
        check("the ack sentence names this worker's agent_id", d and f'"agent_id": "{agent}"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_chat_claim_silent_for_worker_when_flag_disabled():
    def run(td):
        session, agent = "sess-chatclaim-worker-off", "wk10"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(transcript, "The fix lives in docs/TOTALLY_UNSEEN_FILE.md.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("worker Stop stays fully dark without the flag, even with a clear chat-claim signature", out == "", out)
    with_temp_verdicts(run)


# ---- mechanical/unverified-done-claim (DESIGN.md §2h.6(c)) ----


def test_done_claim_fires_on_mutation_without_verification():
    def run(td):
        session = "sess-doneclaim-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done. The parser handles both cases and the edge case is covered.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check("done-claim after an unverified edit fires", d is not None and d.get("decision") == "block", out)
        check("reason carries the done-claim move id", d and 'move="mechanical/unverified-done-claim"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_done_claim_silent_when_turn_ran_a_test():
    def run(td):
        session = "sess-doneclaim-tested"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done. The parser handles both cases and the tests pass.",
            [
                ("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"}),
                ("Bash", {"command": "cargo test -p manifold-core --lib"}),
            ],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a verification-class event (test-run) in the turn suppresses the fire", out == "", out)
    with_temp_verdicts(run)


def test_done_claim_silent_when_turn_read_a_render():
    def run(td):
        session = "sess-doneclaim-render"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Fixed. The gradient banding is gone in the output.",
            [
                ("Edit", {"file_path": "src/shader.rs", "old_string": "a", "new_string": "b"}),
                ("Read", {"file_path": "/tmp/render_out.png"}),
            ],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a render-read in the turn counts as verification — silent", out == "", out)
    with_temp_verdicts(run)


def test_done_claim_never_fires_on_git_only_turn():
    def run(td):
        session = "sess-doneclaim-gitonly"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Pushed. Both commits are on the branch.",
            [
                ("Bash", {"command": "git -C /repo commit -m 'x' -- file.rs"}),
                ("Bash", {"command": "git -C /repo push origin feat/x"}),
            ],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a commit/push-only turn never fires (git's own output verifies the git claim)", out == "", out)
    with_temp_verdicts(run)


def test_done_claim_never_fires_without_mutating_work():
    def run(td):
        session = "sess-doneclaim-readonly"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done. The audit found nothing out of place.",
            [
                ("Read", {"file_path": "src/lib.rs"}),
                ("Bash", {"command": "rg 'pattern' src/"}),
            ],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("reads and read-only bash are not mutating work — silent", out == "", out)
    with_temp_verdicts(run)


def test_done_claim_mutating_bash_counts_as_work():
    def run(td):
        session = "sess-doneclaim-mutbash"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done. The fixture layout is in place.",
            [("Bash", {"command": "mkdir -p fixtures/audio && cp /tmp/a.wav fixtures/audio/"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "a non-git, non-read-only bash command is mutating work — fires",
            d is not None and 'move="mechanical/unverified-done-claim"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_done_claim_confession_skips():
    def run(td):
        session = "sess-doneclaim-confess"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done, but unverified — the render check is still owed.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a final text already confessing unverified-ness never fires", out == "", out)
    with_temp_verdicts(run)


def test_done_claim_requires_leading_claim_words():
    def run(td):
        session = "sess-doneclaim-midsentence"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "The build is done and everything landed cleanly.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check(
            "mid-sentence completion words don't match — leading words or standalone sentences only (crude by design)",
            out == "",
            out,
        )
    with_temp_verdicts(run)


def test_done_claim_loses_priority_to_announced_not_started():
    def run(td):
        session = "sess-doneclaim-priority-announce"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Fixed. Starting the follow-up cleanup now.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "announced-not-started wins when a turn matches both signatures",
            d is not None and 'move="mechanical/announced-not-started"' in d.get("reason", ""),
            out,
        )
        check("done-claim did not also fire in the same block", d and 'move="mechanical/unverified-done-claim"' not in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_done_claim_loses_priority_to_pending_whisper():
    def run(td):
        session = "sess-doneclaim-priority-pending"
        transcript = os.path.join(td, "t.jsonl")
        write_verdict(td, session, "anchor/circling", seq=1)
        write_done_claim_transcript(
            transcript,
            "Done. The parser handles both cases.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "an already-pending whisper wins over the done-claim check",
            d is not None and 'move="anchor/circling"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_done_claim_applies_to_worker_when_flag_enabled():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-doneclaim-worker", "wk11"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done. The parser handles both cases.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "done-claim reaches a worker Stop event when the worker-nudges flag is on",
            d is not None and 'move="mechanical/unverified-done-claim"' in d.get("reason", ""),
            out,
        )
        check("the ack sentence names this worker's agent_id", d and f'"agent_id": "{agent}"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_done_claim_silent_for_worker_when_flag_disabled():
    def run(td):
        session, agent = "sess-doneclaim-worker-off", "wk12"
        transcript = os.path.join(td, "t.jsonl")
        write_done_claim_transcript(
            transcript,
            "Done. The parser handles both cases.",
            [("Edit", {"file_path": "src/parser.rs", "old_string": "a", "new_string": "b"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("worker Stop stays fully dark without the flag, even with a clear done-claim signature", out == "", out)
    with_temp_verdicts(run)


def test_agent_id_routes_to_agent_mailbox():
    def run(td):
        session, agent = "sess-agent", "abc123"
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        write_verdict(td, f"{session}.{agent}", "anchor/circling", seq=1)
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": "/dev/null"}, td)
        d = json.loads(out) if out else None
        check("agent-tagged Stop reads the agent's own mailbox", d is not None and d.get("decision") == "block", out)
        check("agent mailbox consumed marker written under session.agent key", valve.read_consumed(f"{session}.{agent}") == 1)
        check("session-level mailbox untouched", valve.read_consumed(session) == 0)
        # DESIGN.md §2h.4: build_block's supervised ack must carry this
        # worker's agent_id too, since (session_id, seq) alone collides
        # across workers (RUNBOOK.md step 2).
        check("delivered ack sentence names this worker's agent_id", d and f'"agent_id": "{agent}"' in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_agent_id_silent_when_worker_nudges_disabled():
    def run(td):
        session, agent = "sess-agent-off", "def456"
        write_verdict(td, f"{session}.{agent}", "anchor/circling", seq=1)
        check("worker-nudges flag absent in this temp dir", not valve.worker_nudges_enabled())
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": "/dev/null"}, td)
        check("agent-tagged Stop stays dark without the flag", out == "", out)
    with_temp_verdicts(run)


def test_malformed_stdin_and_missing_transcript_exit_clean():
    def run(td):
        out1 = run_hook("not json at all {{{", td)
        check("malformed stdin JSON -> silent", out1 == "", out1)
        out2 = run_hook({"session_id": "sess-no-transcript", "prompt_id": "p1", "transcript_path": "/no/such/file.jsonl"}, td)
        check("missing transcript file -> silent", out2 == "", out2)
        out3 = run_hook({"prompt_id": "p1"}, td)
        check("missing session_id -> silent", out3 == "", out3)
    with_temp_verdicts(run)


def test_stale_stopblock_sentinel_is_swept():
    def run(td):
        session = "sess-sweep"
        stale = os.path.join(td, f"{session}.stopblock.old")
        open(stale, "w").close()
        old_time = time.time() - (25 * 60 * 60)
        os.utime(stale, (old_time, old_time))
        # Any invocation triggers the sweep, even one with nothing to block.
        run_hook({"session_id": "sess-unrelated", "prompt_id": "p9", "transcript_path": "/dev/null"}, td)
        check("stale sentinel (>1 day) removed by the sweep", not os.path.exists(stale))
    with_temp_verdicts(run)


def test_real_hook_process_smoke():
    """One true end-to-end run through the actual subprocess (not the
    in-process exec used by the rest of this file), against a throwaway
    session id in the REAL repo verdicts dir, to catch anything the
    in-process harness's module reload could paper over (import errors,
    syntax errors, path resolution)."""
    fake_session = "test-session-for-stop-hook-smoke"
    real_verdicts = os.path.join(DAEMON_DIR, "verdicts")
    os.makedirs(real_verdicts, exist_ok=True)
    payload = json.dumps({"session_id": fake_session, "prompt_id": "p1", "transcript_path": "/dev/null"})
    r = subprocess.run([sys.executable, str(HOOK_PATH)], input=payload, capture_output=True, text=True)
    check("real subprocess exits 0", r.returncode == 0, r.returncode)
    check("real subprocess prints nothing for a session with no verdict", r.stdout.strip() == "", r.stdout)
    for suffix in (".json", ".consumed", f".stopblock.p1"):
        try:
            os.remove(os.path.join(real_verdicts, f"{fake_session}{suffix}"))
        except OSError:
            pass


# ---- catch-up Stop-wait (re-ruled 2026-07-05: wait for the observer's
# .offset heartbeat to reach the transcript size at Stop, instead of the
# old classifying-marker race) ----


def plant_observer(td, session, drained_offset):
    """A 'live' observer for the hook's liveness gate: a pid file holding
    OUR pid (always alive) plus a heartbeat at the given offset."""
    with open(os.path.join(td, f"{session}.pid"), "w", encoding="utf-8") as f:
        f.write(str(os.getpid()))
    with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
        f.write(str(drained_offset))


def plant_observation_prompt_fired(td, session, agent_id=None):
    """Pre-consume the (unrelated) observation-review-prompt's one-shot
    sentinel so a test can exercise another Stop mechanism in isolation on a
    long (>=40-event, or >=WORKER_ACTIVITY_MIN_EVENTS for a worker) transcript
    without the prompt also firing. `agent_id` targets a worker's own
    sentinel (DESIGN.md §2h.4) instead of the main session's."""
    key = f"{session}.{agent_id}" if agent_id else session
    open(os.path.join(td, f"{key}.observation-prompt-fired"), "w").close()


def plant_grade_backstop_fired(td, session, agent_id=None):
    """Pre-consume the grade-backstop's own one-shot sentinel for a mailbox
    (DESIGN.md §2h.4: a worker's own key when agent_id is given), so a test
    can isolate the observation prompt without the backstop also firing."""
    key = f"{session}.{agent_id}" if agent_id else session
    open(os.path.join(td, f"{key}.grade-backstop-fired"), "w").close()


def test_stop_wait_delivers_verdict_when_observer_catches_up():
    def run(td):
        session = "sess-catchup"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "The fix is in and the tests pass.")
        size = os.path.getsize(transcript)
        plant_observer(td, session, 0)  # observer alive but behind
        # Simulate the observer draining the turn-final text 0.5s in: verdict
        # first (written inside the drain), then the heartbeat catches up.
        import threading

        def observer_drains():
            time.sleep(0.5)
            write_verdict(td, session, "anchor/verify-claim", seq=1)
            with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
                f.write(str(size))

        t = threading.Thread(target=observer_drains)
        t.start()
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        t.join()
        d = json.loads(out) if out else None
        check("catch-up wait delivered the landing verdict", d and d.get("decision") == "block", out)
        check("catch-up wait was short", elapsed < 3.0, elapsed)

    with_temp_verdicts(run)


def test_stop_wait_fast_when_observer_already_caught_up():
    def run(td):
        session = "sess-caughtup"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, os.path.getsize(transcript))
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw2", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("caught-up observer: no wait", elapsed < 0.5, elapsed)
        check("caught-up observer: no block", not out, out)

    with_temp_verdicts(run)


def test_stop_wait_skips_when_observer_dead_or_no_heartbeat():
    def run(td):
        # Dead observer (no pid file) with a lagging heartbeat: no wait.
        session = "sess-deadobs"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
            f.write("0")
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw3", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("dead observer: no wait", elapsed < 0.5, elapsed)
        check("dead observer: no block", not out, out)

        # Live pid but no heartbeat file (pre-heartbeat observer): no wait.
        session2 = "sess-nohb"
        with open(os.path.join(td, f"{session2}.pid"), "w", encoding="utf-8") as f:
            f.write(str(os.getpid()))
        start = time.time()
        out2 = run_hook({"session_id": session2, "prompt_id": "pw4", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("missing heartbeat: no wait", elapsed < 0.5, elapsed)
        check("missing heartbeat: no block", not out2, out2)

    with_temp_verdicts(run)


def test_stop_wait_bounded_when_observer_stalls():
    def run(td):
        session = "sess-stall"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, 0)  # alive, heartbeat never moves
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pw5", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        # Cap raised 6.0 -> 10.0 (§2h.6(b)); the stable-file double-stat
        # (§2h.6(a)) adds one ~0.2s gap before the deadline is armed, hence
        # the slack on both bounds.
        check("stalled observer: returns within cap + slack", elapsed < 12.0, elapsed)
        check("stalled observer: waited to the cap, not less", elapsed > 9.5, elapsed)
        check("stalled observer: no block", not out, out)

    with_temp_verdicts(run)


# ---- snapshot-race fix (§2h.6(a)): stat the transcript twice ~200ms apart
# at Stop, target = max, one extra re-stat if still growing. The defect this
# kills: the final text's write raced the single Stop-entry stat, the stale
# size was already covered by the heartbeat, and the hook skipped the wait
# with the observer alive (5 of 17 late fires in the forensics run). ----


def test_stop_wait_snapshot_race_growth_during_gap_triggers_wait():
    def run(td):
        session = "sess-snaprace"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Setup turn text, not the final one.")
        stale_size = os.path.getsize(transcript)
        # Observer alive and caught up to the STALE size — exactly the
        # VERDICT-AFTER-TURN defect state: pre-fix, a single stat at Stop
        # entry returns stale_size, drained >= target holds immediately, and
        # the hook skips the wait without ever seeing the verdict below.
        plant_observer(td, session, stale_size)
        import threading

        def late_final_text_then_verdict():
            # The harness flushes the turn-final text ~100ms after Stop
            # fires — inside the fix's ~200ms stat gap.
            time.sleep(0.1)
            with open(transcript, "a", encoding="utf-8") as f:
                f.write(json.dumps({
                    "type": "assistant",
                    "timestamp": "2026-07-04T00:00:09Z",
                    "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [{"type": "text", "text": "The fix is in and the tests pass."}]},
                }) + "\n")
            # The observer drains the new text: verdict first, heartbeat after.
            time.sleep(0.3)
            write_verdict(td, session, "anchor/verify-claim", seq=1)
            with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
                f.write(str(os.path.getsize(transcript)))

        t = threading.Thread(target=late_final_text_then_verdict)
        t.start()
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "psr1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        t.join()
        d = json.loads(out) if out else None
        check("growth during the stat gap engages the wait and delivers the verdict", d is not None and d.get("decision") == "block", out)
        check("race-fix delivery is prompt, not a cap wait", elapsed < 3.0, elapsed)

    with_temp_verdicts(run)


def test_stop_wait_stable_file_pays_exactly_one_stat_gap():
    def run(td):
        session = "sess-stablestat"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, os.path.getsize(transcript))  # caught up
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pss1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("stable file: the one ~200ms stat gap was actually paid", elapsed >= 0.15, elapsed)
        check("stable file: no latency beyond the single gap", elapsed < 0.6, elapsed)
        check("stable file: no block", not out, out)

    with_temp_verdicts(run)


def test_stop_wait_skip_paths_pay_no_stat_gap():
    def run(td):
        # Dead observer: the §2h.6(a) double-stat sits BEHIND the liveness
        # gate, so skipped sessions must not pay even the single 200ms gap.
        session = "sess-deadobs-nogap"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
            f.write("0")
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "png1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        check("dead observer: no stat gap paid", elapsed < 0.15, elapsed)
        check("dead observer: no block", not out, out)

    with_temp_verdicts(run)


def test_stop_wait_never_runs_for_workers():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-waitagent", "agw1"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, 0)  # lagging heartbeat must not matter
        start = time.time()
        out = run_hook(
            {"session_id": session, "agent_id": agent, "prompt_id": "pw6", "transcript_path": transcript}, td
        )
        elapsed = time.time() - start
        check("worker stop never waits", elapsed < 0.5, elapsed)
        check("worker stop no block", not out, out)

    with_temp_verdicts(run)


# ---- grade-backstop (2026-07-05 review): sessions never write self-grades
# at all, so the sleep pass has nothing to join fires against. Direct helper
# tests first, then full hook runs. ----


def test_session_gradeable_fires_filters_prefix_agent_and_null_seq():
    with tempfile.TemporaryDirectory() as td:
        tpath = os.path.join(td, "telemetry.jsonl")
        with open(tpath, "w", encoding="utf-8") as f:
            for rec in (
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "anchor/verify-claim", "seq": 2, "ts": 100},
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "mechanical/reasoning-primer", "seq": 1, "ts": 50},
                {"event": "injected", "session_id": "s1", "agent_id": "w1", "move_id": "anchor/thrash", "seq": 3, "ts": 150},
                {"event": "injected", "session_id": "s2", "agent_id": None, "move_id": "coaching/define-done", "seq": 1, "ts": 10},
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "escalate/checkpoint", "seq": 5, "ts": 5},
                {"event": "injected", "session_id": "s1", "agent_id": None, "move_id": "anchor/skim", "seq": None, "ts": 9},
                {"event": "observer_spawn", "session_id": "s1", "ts": 1},
            ):
                f.write(json.dumps(rec) + "\n")
        fires = DIRECT._session_gradeable_fires(tpath, "s1")
        check(
            "keeps only s1's own main-mailbox anchor/coaching/escalate fires with a seq, oldest first",
            [m for _, m, _ in fires] == ["anchor/verify-claim", "escalate/checkpoint"],
            fires,
        )


def test_session_grade_count_sums_across_live_grades_files():
    with tempfile.TemporaryDirectory() as td:
        eval_dir = os.path.join(td, "eval")
        os.makedirs(eval_dir)
        with open(os.path.join(eval_dir, "live_grades.jsonl"), "w", encoding="utf-8") as f:
            f.write(json.dumps({"session_id": "s1"}) + "\n")
        with open(os.path.join(eval_dir, "live_grades.session.jsonl"), "w", encoding="utf-8") as f:
            f.write(json.dumps({"session_id": "s1"}) + "\n")
            f.write(json.dumps({"session_id": "s2"}) + "\n")
        check("counts s1 records across both files", DIRECT._session_grade_count(eval_dir, "s1") == 2)
        check("missing eval dir returns 0, never raises", DIRECT._session_grade_count(os.path.join(td, "nope"), "s1") == 0)


def test_events_since_counts_tool_results_strictly_after_ts():
    with tempfile.TemporaryDirectory() as td:
        path = os.path.join(td, "t.jsonl")
        base = 1783200000
        write_transcript_with_events(path, 5, base)
        check("counts all events after an early since_ts", DIRECT._events_since(path, base) == 5)
        check("counts only events strictly after a later since_ts", DIRECT._events_since(path, base + 3) == 2)
        check("since_ts=None -> 0, never raises", DIRECT._events_since(path, None) == 0)


def test_grade_backstop_fires_when_ungraded_and_stale():
    def run(td):
        session = "sess-backstop-fire"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d1 = json.loads(out1) if out1 else None
        check("backstop blocks when stale and ungraded", d1 is not None and d1.get("decision") == "block", out1)
        check("reason names the backstop move", d1 and 'move="mechanical/grade-backstop"' in d1.get("reason", ""), d1)
        check("reason states the ungraded count", d1 and "1 gradeable" in d1.get("reason", ""), d1)
        check("session-level sentinel written", os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")))

        # Isolate: the (unrelated) observation-review prompt also clears its
        # own >=40-event gate on this transcript — pre-consume its sentinel
        # so this assertion is about the grade backstop alone.
        plant_observation_prompt_fired(td, session)
        out2 = run_hook({"session_id": session, "prompt_id": "p2", "transcript_path": transcript}, td)
        check("second turn stays silent (once per session, not per turn)", out2 == "", out2)

    with_temp_verdicts(run)


def test_grade_backstop_skips_when_not_stale_enough():
    def run(td):
        session = "sess-backstop-fresh"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 10, base)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("backstop stays silent when the oldest fire isn't stale yet", out == "", out)
        check("no sentinel written when it doesn't fire", not os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")))

    with_temp_verdicts(run)


def test_grade_backstop_skips_when_already_graded():
    def run(td):
        session = "sess-backstop-graded"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        write_grade_records([{"session_id": session, "seq": 1, "move_id": "anchor/verify-claim", "correct": True, "effective": True, "grader": "session"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session)  # isolate: unrelated 40-event gate
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("backstop stays silent once the fire is graded", out == "", out)

    with_temp_verdicts(run)


def test_grade_backstop_never_fires_for_worker_stop():
    """A telemetry record attributed to the MAIN session's own mailbox
    (agent_id=None) must never leak into a worker's grade backstop — the two
    mailboxes are scored independently (DESIGN.md §2h.4). Isolates the
    (unrelated, and now worker-reachable — see the §2h.4 tests below)
    observation prompt so this assertion is about backstop cross-attribution
    alone."""
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-backstop-worker", "wk1"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/verify-claim"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session, agent_id=agent)  # isolate: worker's own 20-event gate
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("a main-session-attributed fire never triggers a worker's own grade backstop", out == "", out)

    with_temp_verdicts(run)


def test_worker_grade_backstop_fires_for_own_attributed_fires():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-worker-backstop-fire", "wk2"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": agent, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/thrash"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 25, base)  # > WORKER_ACTIVITY_MIN_EVENTS (20), < main's 40
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check("a worker's own ungraded gradeable fire trips its own backstop", d is not None and d.get("decision") == "block", out)
        check("reason names the backstop move", d and 'move="mechanical/grade-backstop"' in d.get("reason", ""), d)
        check("reason tells the worker to include agent_id on its grade line", d and f'"agent_id": "{agent}"' in d.get("reason", ""), d)
        check(
            "worker gets its own per-(session, agent_id) sentinel",
            os.path.exists(os.path.join(td, f"{session}.{agent}.grade-backstop-fired")),
        )
        check(
            "the main session's own sentinel is untouched",
            not os.path.exists(os.path.join(td, f"{session}.grade-backstop-fired")),
        )

    with_temp_verdicts(run)


def test_worker_grade_backstop_skips_when_own_fires_already_graded():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-worker-backstop-graded", "wk3"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": agent, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/thrash"}]
        )
        write_grade_records([{"session_id": session, "agent_id": agent, "seq": 1, "move_id": "anchor/thrash", "correct": True, "effective": True, "grader": "session"}])
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 25, base)
        plant_observation_prompt_fired(td, session, agent_id=agent)  # isolate: unrelated 20-event gate
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("a worker's own grade line silences its own backstop", out == "", out)

    with_temp_verdicts(run)


def test_worker_grade_backstop_uses_lower_activity_threshold_than_main():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-worker-backstop-threshold", "wk4"
        base = 1783200000
        write_telemetry_records(
            [{"ts": base, "session_id": session, "agent_id": agent, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "anchor/thrash"}]
        )
        transcript = os.path.join(td, "t.jsonl")
        # 25 events: below the main session's 40-event staleness gate, above
        # the worker's own (lower) 20-event one — proves the threshold really
        # differs, not just that some threshold exists.
        write_transcript_with_events(transcript, 25, base)
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check("25 events is stale enough for a worker's own 20-event gate", d is not None and d.get("decision") == "block", out)

    with_temp_verdicts(run)


def test_grade_backstop_ignores_non_gradeable_move_families():
    def run(td):
        session = "sess-backstop-advice-only"
        base = 1783200000
        write_telemetry_records(
            [
                {"ts": base, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 1, "move_id": "mechanical/reasoning-primer"},
                {"ts": base + 1, "session_id": session, "agent_id": None, "event": "injected", "valve": "PostToolUse", "seq": 2, "move_id": "phase/no-verify-before-reporting"},
            ]
        )
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, base)
        plant_observation_prompt_fired(td, session)  # isolate: unrelated 40-event gate
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("advice/phase fires never require a self-grade", out == "", out)

    with_temp_verdicts(run)


# ---- observation review prompt (Peter, 2026-07-05): a standing invitation
# to log anything worth the next sleep pass's attention. Fires once per
# session, only once the session has done enough to be worth reviewing —
# never a repeated demand, since most sessions have nothing to add. ----


def test_observation_prompt_stays_silent_on_a_short_session():
    def run(td):
        session = "sess-obs-short"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 5, 1783200000)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a short session (< 40 events) isn't asked yet", out == "", out)
        check("no sentinel written when it doesn't fire", not os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")))

    with_temp_verdicts(run)


def test_observation_prompt_fires_once_on_a_substantial_session():
    def run(td):
        session = "sess-obs-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, 1783200000)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d1 = json.loads(out1) if out1 else None
        check("substantial session gets asked", d1 is not None and d1.get("decision") == "block", out1)
        check("reason names the observation-prompt move", d1 and 'move="mechanical/observation-prompt"' in d1.get("reason", ""), d1)
        check("reason says it asks once, not every turn", d1 and "once per session" in d1.get("reason", ""), d1)
        check("reason never demands a filler record", d1 and "no action needed" in d1.get("reason", ""), d1)
        check("sentinel written", os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")))

        out2 = run_hook({"session_id": session, "prompt_id": "p2", "transcript_path": transcript}, td)
        check("second turn stays silent — one ask per session, no logged record required", out2 == "", out2)

    with_temp_verdicts(run)


def test_observation_prompt_worker_silent_below_activity_threshold():
    """DESIGN.md §2h.4 extends the observation prompt to workers — it is no
    longer true that a worker Stop event never triggers it (see the
    above-threshold test below). What must still hold: a worker with too
    little activity (< WORKER_ACTIVITY_MIN_EVENTS) isn't asked yet, same
    shape as the main session's own short-session test."""
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-obs-worker-short", "wk1"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 5, 1783200000)
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("a worker below its own 20-event threshold isn't asked yet", out == "", out)
        check(
            "no worker-scoped sentinel written when it doesn't fire",
            not os.path.exists(os.path.join(td, f"{session}.{agent}.observation-prompt-fired")),
        )

    with_temp_verdicts(run)


def test_observation_prompt_fires_for_worker_above_lower_threshold():
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-obs-worker-fire", "wk2"
        transcript = os.path.join(td, "t.jsonl")
        # 25 events: below the main session's 40-event gate, above the
        # worker's own (lower) 20-event one.
        write_transcript_with_events(transcript, 25, 1783200000)
        out1 = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        d1 = json.loads(out1) if out1 else None
        check("a worker above its own lower threshold gets asked", d1 is not None and d1.get("decision") == "block", out1)
        check("reason names the observation-prompt move", d1 and 'move="mechanical/observation-prompt"' in d1.get("reason", ""), d1)
        check("reason still says nothing-to-add is fine — no filler required", d1 and "no action needed" in d1.get("reason", ""), d1)
        check("reason schema mentions this worker's agent_id", d1 and f'"agent_id": "{agent}"' in d1.get("reason", ""), d1)
        check(
            "worker gets its own per-(session, agent_id) sentinel",
            os.path.exists(os.path.join(td, f"{session}.{agent}.observation-prompt-fired")),
        )
        check(
            "the main session's own sentinel is untouched",
            not os.path.exists(os.path.join(td, f"{session}.observation-prompt-fired")),
        )

        out2 = run_hook({"session_id": session, "prompt_id": "p2", "agent_id": agent, "transcript_path": transcript}, td)
        check("second turn for the same worker stays silent — one ask per (session, agent_id)", out2 == "", out2)

    with_temp_verdicts(run)


def test_observation_prompt_worker_sentinel_independent_of_main_session():
    """A worker firing its own observation prompt must never block the main
    session from independently getting its own — own sentinels per mailbox,
    per DESIGN.md §2h.4's 'own sentinels' requirement."""
    def run(td):
        with open(valve.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        session, agent = "sess-obs-worker-vs-main", "wk3"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript_with_events(transcript, 45, 1783200000)  # clears both gates

        out_worker = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        d_worker = json.loads(out_worker) if out_worker else None
        check("worker's own prompt fires first", d_worker is not None and d_worker.get("decision") == "block", out_worker)

        out_main = run_hook({"session_id": session, "prompt_id": "p2", "transcript_path": transcript}, td)
        d_main = json.loads(out_main) if out_main else None
        check("the main session still gets its own independent ask", d_main is not None and d_main.get("decision") == "block", out_main)

    with_temp_verdicts(run)


def main():
    tests = [
        test_pending_flag_blocks_once_and_sentinel_guards_repeat,
        test_stop_hook_active_short_circuits,
        test_announcement_ending_blocks_with_mechanical_payload,
        test_beginning_and_let_me_now_variants_fire,
        test_tool_use_after_text_does_not_fire,
        test_handoff_and_question_endings_do_not_fire,
        test_chat_claim_fires_on_ungrounded_artifact,
        test_chat_claim_silent_when_grounded_by_earlier_tool_input,
        test_chat_claim_tool_output_never_grounds,
        test_chat_claim_silent_for_fenced_artifact,
        test_chat_claim_silent_when_user_echoed_artifact_this_turn,
        test_chat_claim_silent_with_recall_marker,
        test_chat_claim_silent_when_turn_has_any_tool_call,
        test_chat_claim_allcaps_token_resolves_against_real_docs,
        test_chat_claim_allcaps_token_that_does_not_resolve_is_not_an_artifact,
        test_chat_claim_loses_priority_to_announced_not_started,
        test_chat_claim_loses_priority_to_pending_whisper,
        test_chat_claim_applies_to_worker_when_flag_enabled,
        test_chat_claim_silent_for_worker_when_flag_disabled,
        test_done_claim_fires_on_mutation_without_verification,
        test_done_claim_silent_when_turn_ran_a_test,
        test_done_claim_silent_when_turn_read_a_render,
        test_done_claim_never_fires_on_git_only_turn,
        test_done_claim_never_fires_without_mutating_work,
        test_done_claim_mutating_bash_counts_as_work,
        test_done_claim_confession_skips,
        test_done_claim_requires_leading_claim_words,
        test_done_claim_loses_priority_to_announced_not_started,
        test_done_claim_loses_priority_to_pending_whisper,
        test_done_claim_applies_to_worker_when_flag_enabled,
        test_done_claim_silent_for_worker_when_flag_disabled,
        test_agent_id_routes_to_agent_mailbox,
        test_agent_id_silent_when_worker_nudges_disabled,
        test_malformed_stdin_and_missing_transcript_exit_clean,
        test_stale_stopblock_sentinel_is_swept,
        test_real_hook_process_smoke,
        test_stop_wait_delivers_verdict_when_observer_catches_up,
        test_stop_wait_fast_when_observer_already_caught_up,
        test_stop_wait_skips_when_observer_dead_or_no_heartbeat,
        test_stop_wait_bounded_when_observer_stalls,
        test_stop_wait_snapshot_race_growth_during_gap_triggers_wait,
        test_stop_wait_stable_file_pays_exactly_one_stat_gap,
        test_stop_wait_skip_paths_pay_no_stat_gap,
        test_stop_wait_never_runs_for_workers,
        test_session_gradeable_fires_filters_prefix_agent_and_null_seq,
        test_session_grade_count_sums_across_live_grades_files,
        test_events_since_counts_tool_results_strictly_after_ts,
        test_grade_backstop_fires_when_ungraded_and_stale,
        test_grade_backstop_skips_when_not_stale_enough,
        test_grade_backstop_skips_when_already_graded,
        test_grade_backstop_never_fires_for_worker_stop,
        test_worker_grade_backstop_fires_for_own_attributed_fires,
        test_worker_grade_backstop_skips_when_own_fires_already_graded,
        test_worker_grade_backstop_uses_lower_activity_threshold_than_main,
        test_grade_backstop_ignores_non_gradeable_move_families,
        test_observation_prompt_stays_silent_on_a_short_session,
        test_observation_prompt_fires_once_on_a_substantial_session,
        test_observation_prompt_worker_silent_below_activity_threshold,
        test_observation_prompt_fires_for_worker_above_lower_threshold,
        test_observation_prompt_worker_sentinel_independent_of_main_session,
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
