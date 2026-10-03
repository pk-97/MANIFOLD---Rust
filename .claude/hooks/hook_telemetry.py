#!/usr/bin/env python3
"""Runner for every registered hook: one interpreter per event, one telemetry line per hook.

settings.json invokes hooks as

    python3 .../hook_telemetry.py <hook-a.py> [<hook-b.py> ...]

one command per matcher group. Each hook runs in this process via runpy with its own
stdin, stdout, stderr, argv and cwd, and file descriptors 1 and 2 redirected, so a
subprocess a hook spawns cannot write into the harness stream. Why one process: the
hooks themselves take ~1ms; Python startup takes ~25ms, and a tool call used to pay it
eight to eleven times.

Output, one hook: mirrored verbatim, same exit code — behaviorally identical to running
the hook directly.

Output, several hooks, merged the way the harness merges parallel hooks:
  - any exit 2 -> exit 2 with those hooks' stderr (a block is a block);
  - permissionDecision: deny > ask > allow, reasons joined from the winning level;
  - top-level decision: block wins, reasons joined;
  - additionalContext and systemMessage joined; continue=false wins; updatedInput from
    the first hook that sets one;
  - plain-text stdout becomes additionalContext on SessionStart / UserPromptSubmit
    (where plain text is context) and is dropped to stderr elsewhere (where the harness
    never showed it to the model);
  - a hook that crashes fails OPEN: its siblings' verdicts stand and a systemMessage
    names the crash, instead of exit 1 discarding everyone's JSON.

Worker seats never prompt (`worker_no_prompt`): when the payload carries an agent
marker (agent_id / teammate_name / agent_type) and the final PreToolUse verdict is
`ask`, the dispatcher emits `deny` with the original reason plus a reroute note. A
prompt from a worker stalls an unattended run until Peter answers; a deny reaches the
worker, which rewrites or reports up. Lead verdicts are untouched. The harness-level
backstop for prompts the PreToolUse hooks never see is permission-request-guard.py.

Telemetry: one JSONL line per hook to .claude/telemetry/hook-fires.jsonl
({"ts", "hook", "event", "exit", "out", "err", "ms", ...}). "Acted" is out > 0 or
exit != 0. Census: scripts/hook_census.py. Logging never changes a verdict.

Obsolete when: the harness itself reports per-hook invocation/decision telemetry AND
runs command hooks without a process per hook.
"""
import io
import json
import os
import runpy
import sys
import tempfile
import time
import traceback
from datetime import datetime, timezone
from pathlib import Path

_HOOKS_DIR = Path(__file__).resolve().parent
_LOG = _HOOKS_DIR.parent / "telemetry" / "hook-fires.jsonl"
_PLAIN_IS_CONTEXT = ("SessionStart", "UserPromptSubmit")
_PERMISSION_RANK = {"allow": 0, "ask": 1, "deny": 2}


def _derive_decision(stdout: bytes):
    """Best-effort decision label for telemetry. Never raises."""
    if not stdout:
        return None
    try:
        payload = json.loads(stdout)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None
    if not isinstance(payload, dict):
        return None
    hso = payload.get("hookSpecificOutput")
    if isinstance(hso, dict):
        pd = hso.get("permissionDecision")
        if isinstance(pd, str) and pd:
            return pd
        # PermissionRequest hooks answer with {"decision": {"behavior": allow|deny}}.
        dec = hso.get("decision")
        if isinstance(dec, dict) and isinstance(dec.get("behavior"), str):
            return dec["behavior"]
    d = payload.get("decision")
    if isinstance(d, str) and d:
        return d
    if isinstance(hso, dict) and isinstance(hso.get("additionalContext"), str) and hso["additionalContext"]:
        return "context"
    if isinstance(payload.get("additionalContext"), str) and payload["additionalContext"]:
        return "context"
    return None


def _exit_code(code) -> int:
    if code is None:
        return 0
    if isinstance(code, int):
        return code
    print(code, file=sys.stderr)
    return 1


def run_hook(path: Path, stdin_data: bytes):
    """Run one hook script in-process. Returns (exit, stdout bytes, stderr bytes)."""
    saved_streams = (sys.stdin, sys.stdout, sys.stderr)
    saved_argv = sys.argv[:]
    saved_cwd = os.getcwd()
    out_buf, err_buf = io.BytesIO(), io.BytesIO()
    fd_out, fd_err = tempfile.TemporaryFile(), tempfile.TemporaryFile()
    for s in saved_streams[1:]:
        s.flush()
    saved_fd1, saved_fd2 = os.dup(1), os.dup(2)
    os.dup2(fd_out.fileno(), 1)
    os.dup2(fd_err.fileno(), 2)
    sys.stdin = io.TextIOWrapper(io.BytesIO(stdin_data), encoding="utf-8")
    sys.stdout = io.TextIOWrapper(out_buf, encoding="utf-8", write_through=True)
    sys.stderr = io.TextIOWrapper(err_buf, encoding="utf-8", write_through=True)
    sys.argv = [str(path)]
    code = 0
    try:
        runpy.run_path(str(path), run_name="__main__")
    except SystemExit as e:
        code = _exit_code(e.code)
    except BaseException:
        traceback.print_exc()
        code = 1
    finally:
        for s in (sys.stdout, sys.stderr):
            try:
                s.flush()
                s.detach()
            except Exception:
                pass
        sys.stdin, sys.stdout, sys.stderr = saved_streams
        sys.argv = saved_argv
        os.dup2(saved_fd1, 1)
        os.dup2(saved_fd2, 2)
        os.close(saved_fd1)
        os.close(saved_fd2)
        try:
            os.chdir(saved_cwd)
        except OSError:
            pass
    fd_out.seek(0)
    fd_err.seek(0)
    stdout = out_buf.getvalue() + fd_out.read()
    stderr = err_buf.getvalue() + fd_err.read()
    fd_out.close()
    fd_err.close()
    return code, stdout, stderr


def merge(event: str, results: list):
    """Combine several hooks' results into one (exit, stdout, stderr) for the harness."""
    blocking = [r for r in results if r[1] == 2]
    if blocking:
        return 2, b"", b"\n".join(r[3].rstrip() for r in blocking) + b"\n"

    hso: dict = {}
    top: dict = {}
    contexts, messages, stop_reasons = [], [], []
    perm_by_level: dict = {}
    block_reasons, plain, stderr_parts = [], [], []
    for name, code, out, err in results:
        if err.strip():
            stderr_parts.append(err.rstrip())
        if code != 0:
            last = (err.decode("utf-8", "replace").strip().splitlines() or ["no stderr"])[-1]
            messages.append(f"hook {name} failed open (exit {code}): {last}")
            continue
        text = out.decode("utf-8", "replace").strip()
        if not text:
            continue
        try:
            payload = json.loads(text)
        except json.JSONDecodeError:
            payload = None
        if not isinstance(payload, dict):
            plain.append(text)
            continue
        h = payload.get("hookSpecificOutput")
        if isinstance(h, dict):
            pd = h.get("permissionDecision")
            if pd in _PERMISSION_RANK:
                perm_by_level.setdefault(pd, []).append(h.get("permissionDecisionReason") or "")
            if h.get("additionalContext"):
                contexts.append(h["additionalContext"])
            if "updatedInput" in h and "updatedInput" not in hso:
                hso["updatedInput"] = h["updatedInput"]
            for k, v in h.items():
                if k not in ("permissionDecision", "permissionDecisionReason",
                             "additionalContext", "updatedInput", "hookEventName"):
                    hso.setdefault(k, v)
        if payload.get("additionalContext"):
            contexts.append(payload["additionalContext"])
        if payload.get("systemMessage"):
            messages.append(payload["systemMessage"])
        d = payload.get("decision")
        if d == "block":
            top["decision"] = "block"
            block_reasons.append(payload.get("reason") or "")
        elif d and "decision" not in top:
            top["decision"] = d
        if payload.get("continue") is False:
            top["continue"] = False
            if payload.get("stopReason"):
                stop_reasons.append(payload["stopReason"])
        if payload.get("suppressOutput"):
            top["suppressOutput"] = True

    if plain:
        if event in _PLAIN_IS_CONTEXT:
            contexts = plain + contexts
        else:
            stderr_parts.extend(p.encode() for p in plain)

    if perm_by_level:
        level = max(perm_by_level, key=_PERMISSION_RANK.__getitem__)
        hso["permissionDecision"] = level
        reasons = [r for r in perm_by_level[level] if r]
        if reasons:
            hso["permissionDecisionReason"] = "\n\n".join(reasons)
    if contexts:
        hso["additionalContext"] = "\n\n".join(contexts)
    if hso:
        hso["hookEventName"] = event
        top["hookSpecificOutput"] = hso
    if block_reasons:
        top["reason"] = "\n\n".join(r for r in block_reasons if r)
    if stop_reasons:
        top["stopReason"] = "\n".join(stop_reasons)
    if messages:
        top["systemMessage"] = "\n".join(messages)

    stdout = b""
    if top:
        stdout = json.dumps(top).encode()
    stderr = b"\n".join(stderr_parts) + (b"\n" if stderr_parts else b"")
    return 0, stdout, stderr


def _telemetry(stdin_data: bytes, name: str, code: int, out: bytes, err: bytes, ms: int):
    try:
        event = ""
        seat = {}
        if stdin_data:
            try:
                payload = json.loads(stdin_data)
                event = payload.get("hook_event_name", "")
                # Seat attribution: payloads carry the PARENT transcript for teammates,
                # so record the fields that could discriminate seats.
                for k in ("session_id", "teammate_name", "team_name", "tool_name",
                          "agent_id", "agent_type", "permission_mode"):
                    v = payload.get(k)
                    if v:
                        seat[k] = v
                seat["keys"] = ",".join(sorted(payload.keys()))
                # BUG-0x4w (permission prompts untraceable): record what was about to run.
                # PermissionRequest / PermissionDenied carry the same tool_input, and
                # those are exactly the prompts the PreToolUse log could never show.
                if event in ("PreToolUse", "PermissionRequest", "PermissionDenied"):
                    ti = payload.get("tool_input") or {}
                    cmd = ti.get("command") or ti.get("file_path")
                    if isinstance(cmd, str) and cmd:
                        seat["cmd"] = cmd[:500]
            except (json.JSONDecodeError, AttributeError):
                pass
        record = {
            "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "hook": name, "event": event, "exit": code,
            "out": len(out), "err": len(err), "ms": ms, **seat,
        }
        decision = _derive_decision(out)
        if decision is not None:
            record["decision"] = decision
        _LOG.parent.mkdir(parents=True, exist_ok=True)
        with open(_LOG, "a") as f:
            f.write(json.dumps(record, sort_keys=True) + "\n")
    except Exception:
        pass


_WORKER_KEYS = ("agent_id", "teammate_name", "agent_type")
_WORKER_REROUTE = (
    " [worker seat: an `ask` would prompt Peter and stall the unattended run, so the "
    "dispatcher turned it into a deny. Rewrite the command into a pre-approved shape, "
    "or stop and report this text up to the lead — never work around it.]"
)


def worker_no_prompt(payload: dict, out: bytes) -> bytes:
    """Worker seats never prompt: a final PreToolUse `ask` from a subagent/teammate
    becomes a `deny` carrying the original reason plus the reroute note. Lead
    payloads (no agent marker — same discriminator as permission-request-guard.py) pass
    through untouched. `allow`/`deny`/no-verdict are never changed, so this can only
    narrow what runs. Never raises; on any doubt returns `out` as-is."""
    try:
        if payload.get("hook_event_name") != "PreToolUse":
            return out
        if not any(payload.get(k) for k in _WORKER_KEYS):
            return out
        text = out.decode("utf-8", "replace").strip()
        if not text:
            return out
        obj = json.loads(text)
        hso = obj.get("hookSpecificOutput") if isinstance(obj, dict) else None
        if not isinstance(hso, dict) or hso.get("permissionDecision") != "ask":
            return out
        hso["permissionDecision"] = "deny"
        hso["permissionDecisionReason"] = (hso.get("permissionDecisionReason") or "") + _WORKER_REROUTE
        return json.dumps(obj).encode()
    except Exception:
        return out


def main() -> int:
    names = sys.argv[1:]
    if not names:
        print("hook_telemetry: usage: hook_telemetry.py <hook.py> [<hook.py> ...]",
              file=sys.stderr)
        return 0
    stdin_data = sys.stdin.buffer.read()
    try:
        event = json.loads(stdin_data).get("hook_event_name", "") if stdin_data else ""
    except (json.JSONDecodeError, AttributeError):
        event = ""
    if str(_HOOKS_DIR) not in sys.path:
        sys.path.insert(0, str(_HOOKS_DIR))

    results = []
    for name in names:
        path = _HOOKS_DIR / name
        start = time.time()
        if path.is_file():
            code, out, err = run_hook(path, stdin_data)
        else:
            code, out, err = 1, b"", f"hook file missing: {name}\n".encode()
        _telemetry(stdin_data, name, code, out, err, round((time.time() - start) * 1000))
        results.append((name, code, out, err))

    if len(results) == 1:
        code, out, err = results[0][1:]
    else:
        code, out, err = merge(event, results)
    try:
        payload = json.loads(stdin_data) if stdin_data else {}
    except (json.JSONDecodeError, AttributeError):
        payload = {}
    if isinstance(payload, dict):
        rewritten = worker_no_prompt(payload, out)
        if rewritten is not out:
            _telemetry(stdin_data, "worker_no_prompt", 0, rewritten, b"", 0)
            out = rewritten
    sys.stdout.buffer.write(out)
    sys.stderr.buffer.write(err)
    sys.stdout.buffer.flush()
    sys.stderr.buffer.flush()
    return code


if __name__ == "__main__":
    sys.exit(main())
