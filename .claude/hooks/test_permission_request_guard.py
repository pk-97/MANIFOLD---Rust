#!/usr/bin/env python3
"""Tests for permission-request-guard.py and the dispatcher's worker_no_prompt.

Replays the real commands that prompted Peter 2026-09-29..10-01 (from
.claude/telemetry/hook-fires.jsonl, decision=ask) through:
  1. permission-request-guard.py as a PermissionRequest event — worker payloads must
     deny, lead payloads must stay silent (prompt proceeds);
  2. hook_telemetry.py running preToolUseBash.py — a worker `ask` must come out as
     `deny` with the reroute note, a lead `ask` must stay `ask`.
Also: PermissionDenied is observe-only (no output), and a crashing payload yields no
output (fail open). Run from the staged layout: /tmp/permfix/repo/.claude/hooks/.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

HOOKS = Path(__file__).resolve().parent
GUARD = HOOKS / "permission-request-guard.py"
DISPATCH = HOOKS / "hook_telemetry.py"
LOG = HOOKS.parent / "telemetry" / "hook-fires.jsonl"
REAL_LOG = Path("/Users/peterkiemann/MANIFOLD - Rust/.claude/telemetry/hook-fires.jsonl")

FAILS = []


def check(label, cond, detail=""):
    print(("PASS " if cond else "FAIL ") + label + ("" if cond else f"  [{detail}]"))
    if not cond:
        FAILS.append(label)


def run(argv, payload):
    p = subprocess.run(argv, input=json.dumps(payload), capture_output=True, text=True)
    return p.returncode, p.stdout.strip()


def logged_asks(limit=40):
    """Real prompts: (cmd, seat) for preToolUseBash ask decisions since 2026-09-28."""
    rows = []
    if not REAL_LOG.is_file():
        return rows
    for raw in open(REAL_LOG, "rb"):
        try:
            r = json.loads(raw.decode("utf-8", "replace"), strict=False)
        except Exception:
            continue
        if r.get("hook") == "preToolUseBash.py" and r.get("decision") == "ask" \
                and r.get("ts", "") >= "2026-09-28" and r.get("cmd"):
            rows.append((r["cmd"], "worker" if "agent_id" in r.get("keys", "") else "lead"))
    return rows[-limit:]


def pr_payload(cmd, worker, tool="Bash"):
    p = {"hook_event_name": "PermissionRequest", "tool_name": tool,
         "tool_input": {"command": cmd} if tool == "Bash" else {"file_path": cmd},
         "permission_mode": "auto", "session_id": "t", "cwd": "/tmp",
         "tool_use_id": "toolu_test",
         "permission_suggestions": [{"behavior": "allow", "rule": "Bash(x *)"}]}
    if worker:
        p["agent_id"] = "agent-test-1"
        p["agent_type"] = "lane"
    return p


def main() -> int:
    if LOG.exists():
        LOG.unlink()

    # --- 1. PermissionRequest guard, direct ---
    code, out = run([str(GUARD)], pr_payload("sed -n 'w /tmp/x' f", worker=True))
    d = json.loads(out)["hookSpecificOutput"]["decision"]
    check("worker PermissionRequest -> deny", code == 0 and d["behavior"] == "deny", out[:120])
    check("worker deny carries reroute note", "WORKER SEAT" in d["message"] and "report" in d["message"])
    code, out = run([str(GUARD)], pr_payload("sed -n 'w /tmp/x' f", worker=False))
    check("lead PermissionRequest -> no verdict (prompt proceeds)", code == 0 and out == "", out[:120])
    code, out = run([str(GUARD)], pr_payload("x", worker=True, tool="Edit"))
    check("worker Edit prompt -> deny too (tool-agnostic)", json.loads(out)["hookSpecificOutput"]["decision"]["behavior"] == "deny")
    p = pr_payload("x", worker=True)
    p["hook_event_name"] = "PermissionDenied"
    p["reason"] = "[Self-Modification]"
    code, out = run([str(GUARD)], p)
    check("PermissionDenied is observe-only", code == 0 and out == "", out[:120])
    p = pr_payload("x", worker=False)
    p["teammate_name"] = "k27-lane-3"
    code, out = run([str(GUARD)], p)
    check("teammate_name counts as a worker seat", json.loads(out)["hookSpecificOutput"]["decision"]["behavior"] == "deny")
    pp = subprocess.run([str(GUARD)], input="not json", capture_output=True, text=True)
    check("garbage stdin -> fail open (no output, exit 0)", pp.returncode == 0 and pp.stdout.strip() == "")

    # telemetry lines written
    lines = [json.loads(l) for l in open(LOG)] if LOG.exists() else []
    check("every event logged with prompt_source", len(lines) >= 5 and all("prompt_source" in l for l in lines), str(len(lines)))
    check("lead prompt logged as verdict=prompt", any(l["seat"] == "lead" and l["verdict"] == "prompt" for l in lines))
    check("classifier denial logged with reason", any(l["event"] == "PermissionDenied" and l.get("reason") == "[Self-Modification]" for l in lines))

    # --- 2. Dispatcher: worker ask -> deny through the real preToolUseBash.py ---
    def dispatch(cmd, worker):
        p = {"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": cmd},
             "permission_mode": "auto", "session_id": "t", "cwd": "/Users/peterkiemann/MANIFOLD - Rust"}
        if worker:
            p["agent_id"] = "agent-test-1"
        code, out = run([sys.executable, str(DISPATCH), "preToolUseBash.py"], p)
        if not out:
            return "none", ""
        h = json.loads(out)["hookSpecificOutput"]
        return h.get("permissionDecision", "none"), h.get("permissionDecisionReason", "")

    d, r = dispatch("sed -n 'w /tmp/x' docs/README.md", worker=True)
    check("dispatcher: worker ask -> deny", d == "deny", d)
    check("dispatcher: deny keeps the original reason", "write-file" in r, r[:80])
    check("dispatcher: deny carries reroute", "worker seat" in r, r[:80])
    d, _ = dispatch("sed -n 'w /tmp/x' docs/README.md", worker=False)
    check("dispatcher: lead ask stays ask", d == "ask", d)
    d, _ = dispatch("rg -n foo docs | head", worker=True)
    check("dispatcher: worker allow untouched", d == "allow", d)
    d, _ = dispatch("python3 -c 'print(1)'", worker=True)
    check("dispatcher: worker deny untouched", d == "deny", d)
    tl = [json.loads(l) for l in open(LOG)]
    check("dispatcher logs the rewrite as worker_no_prompt", any(l.get("hook") == "worker_no_prompt" and l.get("decision") == "deny" for l in tl))

    # --- 3. Replay the real logged prompts ---
    asks = logged_asks()
    n_w = n_l = 0
    for cmd, seat in asks:
        code, out = run([str(GUARD)], pr_payload(cmd, worker=(seat == "worker")))
        if seat == "worker":
            n_w += 1
            if not (out and json.loads(out)["hookSpecificOutput"]["decision"]["behavior"] == "deny"):
                FAILS.append(f"real worker prompt not denied: {cmd[:80]}")
        else:
            n_l += 1
            if out:
                FAILS.append(f"real lead prompt got a verdict: {cmd[:80]}")
    check(f"real logged prompts replayed: {n_w} worker -> all deny, {n_l} lead -> all pass-through", n_w + n_l > 0)

    print(f"\n{len(FAILS)} failed")
    for f in FAILS:
        print("  -", f)
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
