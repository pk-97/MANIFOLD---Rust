#!/usr/bin/env python3
"""PreToolUse hook for AskUserQuestion: the daemon's deterministic
pre-question tier (DESIGN.md §2c-ask).

Why this exists, and why it can't be the async observer: on 2026-07-04 an
orchestrator hit a "cheap approximation vs. proper primitive" fork and asked
the user with the shortcut marked "(Recommended)" — the shortcut-as-
recommendation framing CLAUDE.md's fix-at-the-root rule forbids. The daemon
never caught it: a question-wait produces no tool events, so nothing revived
the observer during the 12 minutes it sat idle-exited, and even a live
observer's whisper lands after the question is already blocking the session.
This hook runs synchronously, before the question ever renders.

Behavior: `common.detect_shortcut_fork` screens the question's own option
text (no classifier call — deterministic regex, same tier as
`detect_stopgap_markers`). On a match, deny once per distinct question (a
hash-keyed marker file lets an identical re-ask through — never bounce the
same question twice) with a reason quoting fix-at-the-root. Also revives the
observer via `valve.ensure_observer`, since a question-wait is otherwise a
blind spot for the idle-exit revival path.

Semantic tier — WIRED 2026-07-05 with Peter's explicit sign-off ("Yes for
the semantic ask-gate"). When the regex tier doesn't hit, a SYNCHRONOUS
Haiku call judges the question against three gates — decidable-from-
decisions-already-held, mispriced fork, below the ask threshold. A question
is the one event where a synchronous model call is affordable: rare, already
blocking, and about to pause the human for minutes. Deny-once semantics via
the same bounce marker as the regex tier; every verdict (deny or clear) is
logged to telemetry as `ask_gate` for the next sleep pass. Haiku detects,
never prescribes — deny reasons are pre-authored (sleep-pass-editable only,
like moves.md payloads).

Fails open on any error: this hook must never be able to block a session.
"""
import hashlib
import json
import os
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)

BOUNCE_DIR = os.path.join(DAEMON_DIR, "verdicts", "ask_question_bounced")

# 10s not 5: `claude -p` spawn overhead alone is seconds; against a question
# that pauses the human for minutes this is still free, and a timeout fails
# open. Dial for a later pass if telemetry shows it starving.
ASK_GATE_TIMEOUT_S = 10

ASK_GATE_RUBRIC = """You judge ONE question that an AI coding agent is about to ask its human
operator. Decide whether the question should reach the human at all. You are
a detector: output JSON only, never advice.

Gates (flag the single best match, or clear):
- "decidable": the question re-asks a decision its own text shows is already
  made — an option cites prior approval ("you greenlit", "as approved", "per
  your earlier decision"), or one option merely restates a standing rule or
  design the agent already holds.
- "mispriced": one option is priced with an unresolved unknown ("needs a
  small design fix", "requires more work", "blocked on X") where a single
  careful reply's worth of thinking could plausibly resolve or price X — the
  agent is asking BEFORE spending that thought.
- "trivial": a reversible, operational choice with a conventional default
  (naming, internal layout, ordering of independent work) — no taste, scope,
  product, or destructive stakes.

Calibration, read as law:
1. Default verdict is clear. Most questions are legitimate.
2. Genuine taste, product, scope, or destructive-action questions are ALWAYS
   clear, even if they also smell trivial or decidable.
3. Evidence must be verbatim from the question text. No quote, no flag.
4. Confidence below 0.8 -> output clear.

Output exactly:
{"gate": "clear" | "decidable" | "mispriced" | "trivial", "evidence": "<verbatim quote or null>", "confidence": <0.0-1.0 or null>}"""

# Pre-authored deny reasons (Fable, sleep pass 1). Haiku picks the gate; it
# never writes the message.
ASK_GATE_REASONS = {
    "decidable": (
        "This question re-asks a decision its own option text cites as already "
        "made. Act on the standing decision and continue; re-ask only if "
        "something new genuinely invalidates it — and then say what changed."
    ),
    "mispriced": (
        "One branch of this fork is priced with an unresolved unknown. Spend "
        "one reply's worth of thinking to resolve or price it BEFORE asking — "
        "escalations arrive with options priced. If the unknown survives that "
        "thought, re-ask with what you learned."
    ),
    "trivial": (
        "This is a reversible operational call with a conventional default. "
        "Make the call, note it in one sentence, and continue — asking costs "
        "more than being wrong would."
    ),
}


def _question_hash(questions):
    raw = json.dumps(questions, sort_keys=True, default=str)
    return hashlib.sha256(raw.encode("utf-8", "replace")).hexdigest()


def main():
    try:
        data = json.load(sys.stdin)
        session_id = data.get("session_id")
        tool_input = data.get("tool_input") or {}

        try:
            import valve

            valve.ensure_observer(session_id, data.get("transcript_path"))
        except Exception:
            pass

        import common

        questions = tool_input.get("questions")
        if not questions:
            return

        key = _question_hash(questions)
        os.makedirs(BOUNCE_DIR, exist_ok=True)
        marker_path = os.path.join(BOUNCE_DIR, f"{key}.bounced")
        if os.path.exists(marker_path):
            # Already bounced once — this is the re-ask. Let it through.
            return

        reason = None
        tier = None
        gate = None
        confidence = None
        error = None
        if common.detect_shortcut_fork(tool_input):
            tier = "regex"
            gate = "shortcut-fork"
            reason = (
                "This question offers a cheaper option marked (Recommended) alongside "
                "a proper/root-fix option — the shortcut-as-recommendation framing "
                "CLAUDE.md's fix-at-the-root rule forbids. The root fix is the default "
                "recommendation; if it genuinely can't ship this session, say so "
                "explicitly rather than recommending the stopgap. Proceed with the "
                "root fix, or re-ask only if this is a genuine scope, taste, or "
                "destructive-action call — not a cost tradeoff."
            )
        else:
            # Semantic tier (wired 2026-07-05 with Peter's explicit sign-off).
            # Synchronous because a question is rare and already blocking;
            # any error/timeout/low-confidence verdict falls open.
            tier = "semantic"
            verdict = common.call_classifier(
                ASK_GATE_RUBRIC,
                json.dumps(questions, indent=1, default=str),
                timeout=ASK_GATE_TIMEOUT_S,
            )
            g = verdict.get("gate") if isinstance(verdict, dict) else None
            confidence = verdict.get("confidence") if isinstance(verdict, dict) else None
            error = verdict.get("error") if isinstance(verdict, dict) else "no-verdict"
            gate = g if isinstance(g, str) else None
            if g in ASK_GATE_REASONS and isinstance(confidence, (int, float)) and confidence >= 0.8:
                evidence = verdict.get("evidence")
                quoted = f'\n\n(Flagged text: "{evidence}")' if evidence else ""
                reason = ASK_GATE_REASONS[g] + quoted

        try:
            import valve

            valve.append_telemetry(
                {
                    "ts": time.time(),
                    "session_id": session_id,
                    "event": "ask_gate",
                    "tier": tier,
                    "gate": gate,
                    "confidence": confidence,
                    "error": error,
                    "denied": bool(reason),
                }
            )
        except Exception:
            pass

        if not reason:
            return

        with open(marker_path, "w", encoding="utf-8") as f:
            f.write(session_id or "")

        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason,
                    }
                }
            )
        )
    except Exception:
        return


if __name__ == "__main__":
    main()
    sys.exit(0)
