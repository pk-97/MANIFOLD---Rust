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
        check("the ack sentence names this worker's agent_id", d and f" --agent-id {agent} " in d.get("reason", ""), d)
    with_temp_verdicts(run)


def test_chat_claim_silent_for_worker_when_flag_disabled():
    def run(td):
        session, agent = "sess-chatclaim-worker-off", "wk10"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(transcript, "The fix lives in docs/TOTALLY_UNSEEN_FILE.md.")
        out = run_hook({"session_id": session, "prompt_id": "p1", "agent_id": agent, "transcript_path": transcript}, td)
        check("worker Stop stays fully dark without the flag, even with a clear chat-claim signature", out == "", out)
    with_temp_verdicts(run)


# ---- T3 (2026-07-07): widened artifact vocabulary — bare filenames that
# exist in the repo (moves.md near-miss ef0c8e89), and move-id-shaped tokens
# resolvable against moves.md's own headings. ----


def test_chat_claim_fires_on_existing_bare_filename():
    def run(td):
        session = "sess-chatclaim-barefile-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "moves.md now documents the self-disposal exemption.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "a bare filename that really exists in .claude/daemon/ fires when ungrounded",
            d is not None and d.get("decision") == "block" and 'move="mechanical/ungrounded-chat-claim"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_chat_claim_silent_for_bare_filename_grounded_by_earlier_read():
    def run(td):
        session = "sess-chatclaim-barefile-grounded"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "moves.md now documents the self-disposal exemption.",
            prior_tool_calls=[("Read", {"file_path": "moves.md"})],
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("an earlier Read of the bare filename's path grounds it — stays silent", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_silent_for_nonexistent_bare_filename():
    def run(td):
        session = "sess-chatclaim-barefile-fake"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "notes.md handles this.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a plausible but nonexistent bare filename never becomes a candidate", out == "", out)
    with_temp_verdicts(run)


def test_chat_claim_fires_on_real_move_id_token():
    def run(td):
        session = "sess-chatclaim-moveid-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "mechanical/confessed-stopgap already covers this case.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        d = json.loads(out) if out else None
        check(
            "a real move-id-shaped token resolving against moves.md fires when ungrounded",
            d is not None and d.get("decision") == "block" and 'move="mechanical/ungrounded-chat-claim"' in d.get("reason", ""),
            out,
        )
    with_temp_verdicts(run)


def test_chat_claim_silent_for_fake_move_id_shaped_token():
    def run(td):
        session = "sess-chatclaim-moveid-fake"
        transcript = os.path.join(td, "t.jsonl")
        write_chat_claim_transcript(
            transcript,
            "foo/bar-baz already covers this.",
        )
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": transcript}, td)
        check("a move-id-shaped token that doesn't resolve against real moves.md headings never fires", out == "", out)
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
        check("the ack sentence names this worker's agent_id", d and f" --agent-id {agent} " in d.get("reason", ""), d)
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
        check("delivered ack sentence names this worker's agent_id", d and f" --agent-id {agent} " in d.get("reason", ""), d)
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
    session id, to catch anything the in-process harness's module reload
    could paper over (import errors, syntax errors, path resolution).
    T11: the subprocess gets its own fresh `import valve`, so it's pointed at
    a temp verdicts/telemetry path via env vars rather than the real repo
    state — a parent-process monkeypatch of `valve.VERDICTS_DIR` can't reach
    a child interpreter's own import."""
    fake_session = "test-session-for-stop-hook-smoke"
    with tempfile.TemporaryDirectory() as tmp_verdicts:
        env = {
            **os.environ,
            "DAEMON_VERDICTS_DIR": tmp_verdicts,
            "DAEMON_TELEMETRY_PATH": os.path.join(tmp_verdicts, "telemetry.jsonl"),
        }
        payload = json.dumps({"session_id": fake_session, "prompt_id": "p1", "transcript_path": "/dev/null"})
        r = subprocess.run([sys.executable, str(HOOK_PATH)], input=payload, capture_output=True, text=True, env=env)
        check("real subprocess exits 0", r.returncode == 0, r.returncode)
        check("real subprocess prints nothing for a session with no verdict", r.stdout.strip() == "", r.stdout)


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


# plant_observation_prompt_fired / plant_grade_backstop_fired removed
# 2026-07-15: both moves left this hook (DESIGN.md §2k) — see
# test_userpromptsubmit.py, which owns their sentinel-isolation helpers now.


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


# ---- Stop-wait CONVERT fix (sleep pass 2, PASS2_AGENDA item 1): the hook
# pokes the observer to classify the turn-final window, and stops waiting the
# instant the observer clears the poke (verdict delivered, or no drift) instead
# of burning the full cap. The observer here is SIMULATED by a thread exactly
# as the existing catch-up tests simulate it. ----


def _poke_path(td, session):
    return os.path.join(td, f"{session}.classify-now")


def test_stop_wait_converts_no_fire_turn_breaks_early_when_poke_cleared():
    def run(td):
        session = "sess-convert-nofire"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, 0)  # alive, heartbeat NEVER reaches target
        poke_path = _poke_path(td, session)
        import threading

        def observer_classifies_then_clears():
            # Wait for the hook's poke, then clear it with NO verdict — the
            # observer classified the turn-final text and found no drift.
            for _ in range(400):
                if os.path.exists(poke_path):
                    break
                time.sleep(0.02)
            time.sleep(0.3)
            try:
                os.remove(poke_path)
            except OSError:
                pass

        t = threading.Thread(target=observer_classifies_then_clears)
        t.start()
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pcn1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        t.join()
        # THE conversion: pre-fix this burns the full ~10s cap (heartbeat never
        # reaches target); with the poke-clear early-break it ends promptly.
        check("no-fire turn breaks early on poke-clear, not the full cap", elapsed < 3.0, elapsed)
        check("no-fire turn delivers nothing", not out, out)

    with_temp_verdicts(run)


def test_stop_wait_delivers_when_poke_cleared_with_verdict():
    def run(td):
        session = "sess-convert-fire"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "The fix is in and the tests pass.")
        plant_observer(td, session, 0)
        poke_path = _poke_path(td, session)
        import threading

        def observer_fires_then_clears():
            for _ in range(400):
                if os.path.exists(poke_path):
                    break
                time.sleep(0.02)
            time.sleep(0.3)
            # Verdict written inside the (priority) drain, then the poke cleared.
            write_verdict(td, session, "anchor/verify-claim", seq=1)
            try:
                os.remove(poke_path)
            except OSError:
                pass

        t = threading.Thread(target=observer_fires_then_clears)
        t.start()
        start = time.time()
        out = run_hook({"session_id": session, "prompt_id": "pcf1", "transcript_path": transcript}, td)
        elapsed = time.time() - start
        t.join()
        d = json.loads(out) if out else None
        check("turn-final verdict delivered within the wait", d and d.get("decision") == "block", out)
        check("delivered via the wait, not the cap", elapsed < 3.0, elapsed)

    with_temp_verdicts(run)


def test_stop_wait_writes_poke_for_observer():
    def run(td):
        session = "sess-pokewritten"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        plant_observer(td, session, 0)  # alive, behind -> the wait engages
        poke_path = _poke_path(td, session)
        seen = {"poke": False, "target": None}
        import threading

        def watch_for_poke():
            for _ in range(600):
                if os.path.exists(poke_path):
                    seen["poke"] = True
                    try:
                        with open(poke_path, encoding="utf-8") as f:
                            seen["target"] = int(f.read().strip())
                    except (OSError, ValueError):
                        pass
                    try:
                        os.remove(poke_path)  # let the hook finish fast
                    except OSError:
                        pass
                    return
                time.sleep(0.02)

        t = threading.Thread(target=watch_for_poke)
        t.start()
        run_hook({"session_id": session, "prompt_id": "ppw1", "transcript_path": transcript}, td)
        t.join()
        check("Stop wrote a poke while waiting", seen["poke"], seen)
        check("poke target is the transcript size", seen["target"] == os.path.getsize(transcript), seen)
        check("poke cleaned up after the wait", not os.path.exists(poke_path))

    with_temp_verdicts(run)


def test_stop_wait_no_poke_when_observer_dead():
    def run(td):
        # Heartbeat present but no pid file => dead observer => the wait (and so
        # the poke) is gated off entirely; fail-open, no poke ever written.
        session = "sess-deadnopoke"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "Here is the summary of what I found.")
        with open(os.path.join(td, f"{session}.offset"), "w", encoding="utf-8") as f:
            f.write("0")
        out = run_hook({"session_id": session, "prompt_id": "pdn1", "transcript_path": transcript}, td)
        check("dead observer: no poke written", not os.path.exists(_poke_path(td, session)))
        check("dead observer: no block", not out, out)

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



# ---- grade-backstop and observation-review-prompt tests moved to
# test_userpromptsubmit.py 2026-07-15 (DESIGN.md §2k) — both moves left
# the Stop hook. ----



# ---- advice-kind never blocks Stop + hardened block wording
# (2026-07-15, Peter's ruling — DESIGN.md §2k) ----


HARDENED_SENTENCE = "Address only this note, then end your turn — do not resume or begin other work."


def test_pending_advice_flag_is_never_a_stop_block():
    def run(td):
        session = "sess-advice-noblock"
        # mechanical/design-primer is a real `kind: advice` move in moves.md.
        write_verdict(td, session, "mechanical/design-primer", seq=1)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        check("an advice-kind pending flag never blocks Stop", out == "", out)
        check("advice flag stays unconsumed for PostToolUse/UserPromptSubmit", valve.read_consumed(session) == 0)
        check("no stopblock sentinel written (nothing was blocked)", not os.path.exists(os.path.join(td, f"{session}.stopblock.p1")))
    with_temp_verdicts(run)


def test_pending_alert_flag_still_blocks():
    def run(td):
        session = "sess-alert-blocks"
        # anchor/circling has no explicit `kind:` entry -> defaults to alert.
        write_verdict(td, session, "anchor/circling", seq=1)
        out = run_hook({"session_id": session, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        d = json.loads(out) if out else None
        check("an alert-kind pending flag still blocks Stop", d is not None and d.get("decision") == "block", out)
        check("alert flag is consumed on delivery", valve.read_consumed(session) == 1)
    with_temp_verdicts(run)


def test_block_reasons_end_with_hardened_sentence():
    def run(td):
        # Pending (alert) flag path.
        session1 = "sess-hardened-pending"
        write_verdict(td, session1, "anchor/circling", seq=1)
        out1 = run_hook({"session_id": session1, "prompt_id": "p1", "transcript_path": "/dev/null"}, td)
        d1 = json.loads(out1) if out1 else None
        check(
            "pending-flag block reason ends with the hardened sentence",
            d1 is not None and d1.get("reason", "").rstrip().endswith(HARDENED_SENTENCE),
            d1,
        )

        # Deterministic mechanical/announced-not-started path.
        session2 = "sess-hardened-announce"
        transcript = os.path.join(td, "t.jsonl")
        write_transcript(transcript, "I found the issue. Starting the migration now with the new schema.")
        out2 = run_hook({"session_id": session2, "prompt_id": "p1", "transcript_path": transcript}, td)
        d2 = json.loads(out2) if out2 else None
        check(
            "announced-not-started block reason ends with the hardened sentence",
            d2 is not None and d2.get("reason", "").rstrip().endswith(HARDENED_SENTENCE),
            d2,
        )
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
        test_chat_claim_fires_on_existing_bare_filename,
        test_chat_claim_silent_for_bare_filename_grounded_by_earlier_read,
        test_chat_claim_silent_for_nonexistent_bare_filename,
        test_chat_claim_fires_on_real_move_id_token,
        test_chat_claim_silent_for_fake_move_id_shaped_token,
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
        test_stop_wait_converts_no_fire_turn_breaks_early_when_poke_cleared,
        test_stop_wait_delivers_when_poke_cleared_with_verdict,
        test_stop_wait_writes_poke_for_observer,
        test_stop_wait_no_poke_when_observer_dead,
        test_stop_wait_skip_paths_pay_no_stat_gap,
        test_stop_wait_never_runs_for_workers,
        test_pending_advice_flag_is_never_a_stop_block,
        test_pending_alert_flag_still_blocks,
        test_block_reasons_end_with_hardened_sentence,
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
