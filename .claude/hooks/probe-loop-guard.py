#!/usr/bin/env python3
"""PreToolUse: the lead must review the seam before grinding an iteration loop.

Ladder (doctrine: docs/AGENT_ROUTING.md section Lead token economy): semantic
review first, then a review lane, probes last and delegated. Binds ANY lead
loop, not just kernel-file probes.

Two counters, either one trips the guard.

PROBE — every action counts:
  - Edit/Write/MultiEdit on RT/GPU kernel or shader files (manifold-gpu/metal/**,
    render_scene.rs, *.wgsl under crates/);
  - Bash RUNNING the suite or probe binary (cargo test/run with gpu-proofs, the
    render-import bin, the gate wrapper). Quoted strings are stripped first, so
    naming a marker in a commit message or bead never counts.

LOOP — per file, max across files: one cycle = an edit to a file already edited
this session with an observation run in between (cargo test/t/nextest/run/r, or a
target/debug|release binary). check/clippy/build are compile-fix iteration and
never mark a run. Healthy work spreads edits across files; a grind hammers one.

Read-only git plumbing is exempt: merge-base, rev-parse, log, branch segments are
stripped before matching. For-loop word lists are data, not execution.

At 3: warning. At 6+: deny until the session writes /tmp/manifold_seam_review.md
(>=200 chars, the evidence table), which resets both counters. The review must be
newer than `last_counted_ts` in the state JSON — never the state file mtime, which
bookkeeping touches. last_counted_ts == 0 means no reset, so a stale review from an
earlier session cannot disarm the guard.

LEAD SEAT ONLY — lanes are the delegation target and legitimately run probe loops.
Seat test: subagent payloads carry `agent_id`/`agent_type`, the lead's carries
neither; marker present -> silent. Transcript-model detection is wrong here,
because a teammate payload carries the parent's transcript.

Fails OPEN.

Obsolete when: the debug escalation ladder in docs/AGENT_ROUTING.md is retired.
"""
import json
import os
import re
import sys
import time

REVIEW_FILE = "/tmp/manifold_seam_review.md"
WARN_AT = 3
DENY_AT = 6


def is_worker_seat(payload: dict) -> bool:
    return any(payload.get(k) for k in ("agent_id", "agent_type", "teammate_name"))

KERNEL_PATH = re.compile(r"crates/manifold-gpu/src/metal/|render_scene\.rs$|\.wgsl$")
# A probe is an EXECUTION, not a mention (BUG-q329, Peter-approved 2026-07-29:
# "reclassify probe-loop-guard to execution-shape matching with quote
# stripping"): count only commands that actually run the suite or the probe
# binary. Quoted strings are stripped first, so commit messages and bead
# reasons naming the markers never count. Accepted trade-off: a probe wrapped
# entirely in quotes evades the counter — the guard is fail-open by design.
PROBE_CMD = re.compile(
    r"cargo\s+(?:test|t|run|r)\b[^|;&\n]*?"
    r"(?:--features[^|;&\n]*?gpu[-_]proofs|--bin\s+render-import)"
    r"|\brender-import(?=\s|$)"
)
# An observation run for the generic loop counter: behavior is being watched
# (tests, the app, a built binary). check/clippy/build are compile-fix
# iteration and deliberately excluded — fixing warnings is not a debug loop.
EXEC_CMD = re.compile(
    r"\bcargo\s+(?:test|t|nextest|run|r)\b"
    r"|(?:^|[\s;&|])\.?/?target/(?:debug|release)/\S+"
)
# The gate wrapper's path is normally quoted (the repo path has a space), so
# it must match the RAW command; end-of-token anchor keeps prose mentions
# like "gpu_proofs_gate.py: …" in commit messages from counting.
WRAPPER_RUN = re.compile(r"gpu_proofs_gate\.py['\"]?(?=\s|$)")
QUOTED = re.compile(r"'[^']*'|\"[^\"]*\"")
# Read-only git plumbing is bookkeeping, not probing (BUG-0c28): a landing's
# merge-base/rev-parse/log/branch loops must never feed the probe counter,
# even when branch names or arguments contain probe-marker strings. Strip
# those invocations from the command text before PROBE_CMD matches.
GIT_PLUMBING = re.compile(
    r"\bgit\s+(?:-C\s+(?:\"[^\"]*\"|'[^']*'|\S+)\s+)*"
    r"(?:merge-base|rev-parse|log|branch)\b[^|;&\n]*"
)
# A for-loop's word list is data, never execution — branch names like
# lane/render-import-fix in `for b in …` must not count either. The loop
# BODY (after `do`) still matches normally.
FOR_HEADER = re.compile(r"\bfor\s+\w+\s+in\s+[^;\n]*")


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
        is_exec = False
        edit_path = ""
        if tool in ("Edit", "Write", "MultiEdit"):
            edit_path = ti.get("file_path", "")
            is_probe = bool(KERNEL_PATH.search(edit_path))
        elif tool == "Bash":
            raw = ti.get("command", "")
            cmd = GIT_PLUMBING.sub("", FOR_HEADER.sub("", QUOTED.sub("", raw)))
            is_probe = bool(PROBE_CMD.search(cmd)) or bool(WRAPPER_RUN.search(raw))
            is_exec = is_probe or bool(EXEC_CMD.search(cmd))
        if not (is_probe or is_exec or edit_path):
            return

        if is_worker_seat(payload):
            return  # lane/consult seat — iteration loops are their job

        cp = counter_path(session)
        state = {}
        if os.path.exists(cp):
            try:
                state = json.load(open(cp))
            except Exception:
                pass
        state.setdefault("count", 0)
        state.setdefault("seq", 0)
        state.setdefault("last_exec", 0)
        state.setdefault("edits", {})
        state.setdefault("cycles", {})
        state.setdefault("last_counted_ts", 0.0)

        state["seq"] += 1
        seq = state["seq"]

        # Bookkeeping (never trips the guard on its own).
        if is_exec:
            state["last_exec"] = seq

        counted = False
        if is_probe:
            counted = True
        if edit_path:
            prev = state["edits"].get(edit_path)
            if prev is not None and state["last_exec"] > prev:
                # re-edit after an observation run: one loop cycle
                state["cycles"][edit_path] = int(state["cycles"].get(edit_path, 0)) + 1
                counted = True
            state["edits"][edit_path] = seq

        if not counted:
            json.dump(state, open(cp, "w"))
            return

        # The written review resets the loop — it must be newer than the last
        # COUNTED action. last_counted_ts == 0 means nothing counted yet: a
        # review file left over from an earlier session must NOT reset.
        last_ts = float(state.get("last_counted_ts", 0.0))
        if last_ts > 0.0 and os.path.exists(REVIEW_FILE):
            try:
                if os.path.getsize(REVIEW_FILE) >= 200 and os.path.getmtime(REVIEW_FILE) > last_ts:
                    os.remove(cp)
                    return
            except Exception:
                pass

        if is_probe:
            state["count"] = int(state.get("count", 0)) + 1
        state["last_counted_ts"] = time.time()
        json.dump(state, open(cp, "w"))

        probe_n = int(state.get("count", 0))
        cycle_n = max([int(v) for v in state.get("cycles", {}).values()] or [0])
        n = max(probe_n, cycle_n)
        if n < WARN_AT:
            return

        shape = []
        if probe_n:
            shape.append(f"{probe_n} probe actions")
        if cycle_n:
            shape.append(f"{cycle_n} edit-run-edit cycles on one file")
        msg = (
            f"LOOP GUARD ({' + '.join(shape)} this session) — lead escalation ladder "
            "(Peter 2026-07-25, widened 2026-07-30 to ALL lead iteration loops, not "
            "just probes): (1) LEAD semantic code review of the seam first — "
            "'does this look correct?' is the fastest, cheapest oracle; (2) STUCK? ask "
            "a tool-using GLM review lane for adversarial review (one-shots fabricate code citations, Peter 2026-07-26 — .claude/hooks/oneshot is mechanical-tasks-only) "
            "--model glm-5.2, or a lane); (3) instrument probes are the LAST RESORT, for "
            "when nothing makes sense and you need a new direction — and they are DELEGATED "
            "(DeepSeek lane), not lead-run. Write the evidence table to "
            "/tmp/manifold_seam_review.md (>=200 chars) to reset this guard. "
            "Before the next iteration, run the DEBUG_INVESTIGATION skeleton as a "
            "checklist (docs/archive/SEMANTIC_WORKFLOW_PROGRAMS.md section 10): SCHEMA_SEARCH before "
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
