#!/usr/bin/env python3
"""PreToolUse hook for Workflow: the daemon's launch-announcement +
model-discipline gate (DESIGN.md §2g, launch tier).

Why this exists: on 2026-07-05 Peter caught an Opus orchestrator launching a
workflow whose agent() calls carried no explicit model — every worker
silently inherited the session's Opus tier. The Agent-tool version of this
failure already has an async anchor (anchor/agent-model-discipline) and a
ledger annotation; Workflow scripts were the named gap ("agent() model
choices live inside the script text, not the ledger"). A workflow launch is
rare, already blocking, and damage-preceding — the §2c-ask criteria for a
synchronous gate — so this runs deterministically before the fleet exists,
not as a whisper after it's spent the tokens.

Two tiers, both deterministic (no classifier call):

1. Model discipline — EVERY launch. Each `agent(` call site in the script
   must carry an explicit `model:` in its options. A call without one
   inherits the session model, which is exactly the silent Opus-spawns-Opus
   path. Violations deny with the offending line numbers. Re-checked on
   every retry; there is no bounce-out.

2. Announce-once — per (session, workflow name). The first launch of a
   given workflow is denied once with instructions to announce, in visible
   text to the user: what the workflow is for, why it needs orchestration,
   the fan-out, and the model tier of every stage with a reason. The parsed
   roster is embedded in the deny so the announcement is grounded. The
   relaunch passes. Keyed on the workflow's meta name, NOT a content hash —
   the retry usually edits the script (adding model:), and a content key
   would bounce the fixed script a second time.

A launch that clears both tiers emits NO decision: it falls through to the
normal permission flow. This gate only adds requirements; the §2g bounds
tier (auto-approve within an allowance) is a separate build and the only
thing that may ever widen permissions.

Fails open on any error — a parse failure, unreadable scriptPath, or
unbalanced script never denies. Telemetry: `workflow_gate` records on every
decision, for sleep-pass review.
"""
import json
import os
import re
import sys
import time

HOOKS_DIR = os.path.dirname(os.path.abspath(__file__))
DAEMON_DIR = os.path.normpath(os.path.join(HOOKS_DIR, "..", "daemon"))
sys.path.insert(0, DAEMON_DIR)

BOUNCE_DIR = os.path.join(DAEMON_DIR, "verdicts", "workflow_launch_bounced")

# Pre-authored deny reasons (Fable, 2026-07-05, at Peter's direction). Edits
# are sleep-pass-only, like moves.md payloads.
ANNOUNCE_REASON = (
    "Workflow launch paused — once, not rejected. Before relaunching, announce "
    "this workflow to the user in your visible text: (1) what it is for and why "
    "it needs multi-agent orchestration rather than inline work; (2) the "
    "fan-out — how many agents, what each stage does; (3) the model tier of "
    "every agent() stage, with a reason. Workers in this repo run Sonnet by "
    "default; a stage running Opus or above must say why the stage's difficulty "
    "earns it, and inheriting the session model is never a choice — it is the "
    "silent Opus-spawns-Opus path. Reason about the tiers before you relaunch; "
    "do not restate defaults. Then relaunch — the same workflow passes this "
    "gate once announced."
)

MODEL_REASON = (
    "This script has agent() calls with no explicit model: option. They "
    "inherit the session's model — when the orchestrator is Opus or Fable, "
    "that silently runs every worker at orchestrator tier (caught live "
    "2026-07-04 and again 2026-07-05). Set model: explicitly on EVERY agent() "
    "call — 'sonnet' is the worker default here; choose a heavier tier only "
    "where the stage's difficulty justifies it, and say why in your "
    "announcement. This check runs on every launch; it cannot be waited out."
)


def _strip_strings(src):
    """Replace the contents of '...', "..." and `...` literals with spaces,
    preserving offsets/newlines, so code-level scans don't match prose."""
    out = list(src)
    i, n = 0, len(src)
    quote = None
    while i < n:
        c = src[i]
        if quote:
            if c == "\\":
                if i + 1 < n and src[i + 1] != "\n":
                    out[i + 1] = " "
                out[i] = " "
                i += 2
                continue
            if c == quote:
                quote = None
            elif c != "\n":
                out[i] = " "
        elif c in ("'", '"', "`"):
            quote = c
        i += 1
    return "".join(out)


def _call_span(stripped, open_paren):
    """Given index of the '(' opening a call in string-stripped source,
    return the index one past its matching ')', or None if unbalanced."""
    depth = 0
    for i in range(open_paren, len(stripped)):
        c = stripped[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i + 1
    return None


def parse_agent_calls(script):
    """Return (calls, parse_ok). Each call: {line, model} where model is the
    explicit model value ('inherit' sentinel when absent, '<expr>' when the
    option is present but not a string literal)."""
    stripped = _strip_strings(script)
    calls = []
    for m in re.finditer(r"\bagent\(", stripped):
        open_paren = m.end() - 1
        end = _call_span(stripped, open_paren)
        if end is None:
            return calls, False
        span = stripped[open_paren:end]
        line = script.count("\n", 0, m.start()) + 1
        model = "inherit"
        opt = re.search(r"\bmodel\s*:", span)
        if opt:
            # The value is string-stripped; read it from the original text.
            val = re.match(
                r"\s*['\"]([a-z0-9-]+)['\"]",
                script[open_paren + opt.end() :],
                re.IGNORECASE,
            )
            model = val.group(1) if val else "<expr>"
        calls.append({"line": line, "model": model})
    return calls, True


def _workflow_identity(tool_input):
    """Stable per-workflow key: meta name from the script if present, else
    the saved-workflow name param, else the scriptPath basename."""
    script = tool_input.get("script")
    if isinstance(script, str):
        m = re.search(r"\bname\s*:\s*['\"]([^'\"]+)['\"]", script)
        if m:
            return m.group(1)
    for key in ("name", "scriptPath"):
        v = tool_input.get(key)
        if isinstance(v, str) and v:
            return os.path.basename(v)
    return "unnamed"


def _roster_line(calls, parse_ok):
    if not parse_ok:
        return "Roster: script not statically parseable — enumerate the stages yourself."
    if not calls:
        return "Roster: no agent() call sites found."
    counts = {}
    for c in calls:
        counts[c["model"]] = counts.get(c["model"], 0) + 1
    parts = ", ".join(
        f"{n}x {'NO model: (inherits session tier)' if m == 'inherit' else m}"
        for m, n in sorted(counts.items(), key=lambda kv: -kv[1])
    )
    return f"Roster parsed from the script: {len(calls)} agent() call sites — {parts}."


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

        script = tool_input.get("script")
        if not isinstance(script, str):
            path = tool_input.get("scriptPath")
            if isinstance(path, str):
                try:
                    with open(path, encoding="utf-8", errors="replace") as f:
                        script = f.read()
                except Exception:
                    script = None

        calls, parse_ok = ([], True)
        if isinstance(script, str):
            calls, parse_ok = parse_agent_calls(script)
        unmodeled = [c for c in calls if c["model"] == "inherit"] if parse_ok else []

        identity = _workflow_identity(tool_input)
        slug = re.sub(r"[^A-Za-z0-9_-]", "_", identity)[:80]
        os.makedirs(BOUNCE_DIR, exist_ok=True)
        marker_path = os.path.join(BOUNCE_DIR, f"{session_id}.{slug}.bounced")
        announced = os.path.exists(marker_path)

        reason = None
        tier = "pass"
        if unmodeled:
            tier = "model-deny"
            sites = ", ".join(f"line {c['line']}" for c in unmodeled[:12])
            reason = f"{MODEL_REASON}\n\nCall sites missing model:: {sites}."
            if not announced:
                reason = f"{ANNOUNCE_REASON}\n\n{_roster_line(calls, parse_ok)}\n\n{reason}"
        elif not announced:
            tier = "announce-deny"
            reason = f"{ANNOUNCE_REASON}\n\n{_roster_line(calls, parse_ok)}"

        if reason and not announced:
            with open(marker_path, "w", encoding="utf-8") as f:
                f.write(identity)

        try:
            import valve

            valve.append_telemetry(
                {
                    "ts": time.time(),
                    "session_id": session_id,
                    "event": "workflow_gate",
                    "tier": tier,
                    "workflow": identity,
                    "n_agents": len(calls),
                    "n_unmodeled": len(unmodeled),
                    "parse_ok": parse_ok,
                }
            )
        except Exception:
            pass

        if not reason:
            return

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
