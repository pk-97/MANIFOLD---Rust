#!/usr/bin/env python3
"""probe-loop-guard.py — PreToolUse hook enforcing the review-before-bisect
rule (Peter 2026-07-25): the lead must not run long instrument-probe loops
(debug-hack a kernel, render, stare, repeat) without first writing the
evidence table and reviewing the seam.

Why: the 2026-07-25 BUG-326 hunt found the mechanism (depth snapshot wrong on
imported glb scenes) but burned ~2h of lead context on serial theory loops.
The repo's seam bugs come from eras of weak briefs; review-first is cheaper
than probe-first. Doctrine: docs/AGENT_ROUTING.md §Lead token economy.

Mechanism: counts probe-loop actions per session —
  - Edit/Write/MultiEdit touching RT/GPU kernel or shader files
    (manifold-gpu/metal/**, render_scene.rs, *.wgsl under crates/)
  - Bash running render-import or gpu-proofs tests
At 3: warning (additionalContext). At 6+: DENY until the session writes
/tmp/manifold_seam_review.md (>=200 chars — the evidence table), which
resets the counter. Fails OPEN on any error.

LEAD SEAT ONLY (Peter 2026-07-28): the ladder binds the lead; lanes are the
delegation target and legitimately run probe loops. Caller tier is read from
the payload transcript's last assistant `message.model` (same mechanism as
agent-tier-spawn-guard.py); a non-lead model returns silent. Empty or
unreadable model enforces — the lead is the failure surface.

Obsolete when: the debug escalation ladder in docs/AGENT_ROUTING.md is retired or replaced; this hook is that doctrine's enforcement arm.
"""
import json
import os
import re
import sys
import time

REVIEW_FILE = "/tmp/manifold_seam_review.md"
WARN_AT = 3
DENY_AT = 6

LEAD_TIERS = re.compile(r"fable|claude-opus|kimi-k3", re.IGNORECASE)
TAIL_BYTES = 256 * 1024


def caller_model(transcript_path: str) -> str:
    try:
        with open(transcript_path, "rb") as f:
            try:
                f.seek(-TAIL_BYTES, os.SEEK_END)
            except OSError:
                f.seek(0)
            tail = f.read().decode("utf-8", errors="replace")
    except OSError:
        return ""
    model = ""
    for line in tail.splitlines():
        if '"model"' not in line:
            continue
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        m = (entry.get("message") or {}).get("model") or entry.get("model") or ""
        if isinstance(m, str) and m:
            model = m
    return model

KERNEL_PATH = re.compile(r"crates/manifold-gpu/src/metal/|render_scene\.rs$|\.wgsl$")
PROBE_CMD = re.compile(r"render-import|gpu-proofs|gpu_proofs")


def counter_path(session: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9_-]", "_", session)[:64] or "unknown"
    return f"/tmp/manifold_probe_loop_{safe}.json"


def main() -> None:
    try:
        payload = json.load(sys.stdin)
        tool = payload.get("tool_name", "")
        ti = payload.get("tool_input", {}) or {}
        session = payload.get("session_id", "unknown")

        is_probe = False
        if tool in ("Edit", "Write", "MultiEdit"):
            path = ti.get("file_path", "")
            is_probe = bool(KERNEL_PATH.search(path))
        elif tool == "Bash":
            is_probe = bool(PROBE_CMD.search(ti.get("command", "")))
        if not is_probe:
            return

        model = caller_model(payload.get("transcript_path") or "")
        if model and not LEAD_TIERS.search(model):
            return  # lane/dispatcher seat — probe loops are their job

        cp = counter_path(session)
        # Anchor the reset check BEFORE rewriting the counter: the review must
        # be newer than the last probe action, not newer than right now
        # (rewriting first made the window ~1s — physically unmeetable, the
        # 2026-07-25 deadlock).
        prev_mtime = os.path.getmtime(cp) if os.path.exists(cp) else 0.0
        state = {"count": 0}
        if os.path.exists(cp):
            try:
                state = json.load(open(cp))
            except Exception:
                pass
        state["count"] = int(state.get("count", 0)) + 1
        json.dump(state, open(cp, "w"))
        n = state["count"]

        # The written review resets the loop (newer than the previous action).
        # prev_mtime == 0.0 means no counter existed yet — a review file left
        # over from an earlier session must NOT reset (it deleted the counter
        # on every probe, so the guard never counted past 1: the 2026-07-28
        # silent-disarm, 253 telemetry fires with zero output).
        if prev_mtime > 0.0 and os.path.exists(REVIEW_FILE):
            try:
                if os.path.getsize(REVIEW_FILE) >= 200 and os.path.getmtime(REVIEW_FILE) > prev_mtime:
                    os.remove(cp)
                    return
            except Exception:
                pass

        msg = (
            f"PROBE-LOOP GUARD ({n} probe actions this session) — lead escalation ladder "
            "(Peter 2026-07-25): (1) LEAD semantic code review of the seam first — "
            "'does this look correct?' is the fastest, cheapest oracle; (2) STUCK? ask "
            "a tool-using GLM review lane for adversarial review (one-shots fabricate code citations, Peter 2026-07-26 — .claude/hooks/oneshot is mechanical-tasks-only) "
            "--model glm-5.2, or a lane); (3) instrument probes are the LAST RESORT, for "
            "when nothing makes sense and you need a new direction — and they are DELEGATED "
            "(DeepSeek lane), not lead-run. Write the evidence table to "
            "/tmp/manifold_seam_review.md (>=200 chars) to reset this guard. "
            "Before the next probe, run the DEBUG_INVESTIGATION skeleton as a "
            "checklist (SEMANTIC_WORKFLOW_PROGRAMS.md §10): SCHEMA_SEARCH before "
            "any negative claim; GENERALIZE_TRIGGER after the first repro; "
            "CURE_TEST once a perfect action-correlation exists and two read "
            "rounds haven't cracked the mechanism."
        )
        if n >= DENY_AT:
            print(json.dumps({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": msg,
                }
            }))
        elif n == WARN_AT:
            print(json.dumps({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "additionalContext": msg,
                }
            }))
    except Exception:
        # fail open — never block a session on a guard bug
        return


if __name__ == "__main__":
    main()
