#!/usr/bin/env python3
"""Read Codex session jsonl files and report token usage (stdlib only)."""
import argparse, datetime as dt, hashlib, json, os, pathlib, shlex, sys
from urllib.parse import unquote, urlparse
from collections import Counter, defaultdict

def _num(d, *names):
    for n in names:
        v = d.get(n)
        if isinstance(v, (int, float)): return v
    return 0

def _time(v):
    if not isinstance(v, str): return None
    try:
        parsed = dt.datetime.fromisoformat(v.replace("Z", "+00:00"))
        return parsed.replace(tzinfo=dt.timezone.utc) if parsed.tzinfo is None else parsed
    
    except ValueError: return None

def _label(command):
    text = str(command).strip().split()
    return text[0] if text else "unknown"

def _in_bounds(timestamp, since, until):
    value = _time(timestamp)
    if value is None:
        value = dt.datetime.min.replace(tzinfo=dt.timezone.utc)
    return (since is None or value >= since) and (until is None or value < until)

def _resolved(path):
    if not path: return ""
    if str(path).startswith("file:"):
        path = unquote(urlparse(str(path)).path)
    return str(pathlib.Path(path).expanduser().resolve())

def _command_text(command):
    if isinstance(command, str): return command
    if not isinstance(command, list): return ""
    argv = [str(v) for v in command]
    if len(argv) >= 3 and pathlib.Path(argv[0]).name in ("sh", "bash", "zsh", "fish", "dash") and argv[-2] in ("-c", "-lc", "--command"):
        return argv[-1]
    return shlex.join(argv)

def _in_repo(cwd, repo):
    if not repo: return True
    try: return bool(cwd) and pathlib.Path(cwd).resolve().is_relative_to(pathlib.Path(repo).expanduser().resolve())
    except (OSError, ValueError): return False

def scan(sessions_dir, since=None, repo=None, *, until=None):
    root = pathlib.Path(os.path.expanduser(sessions_dir))
    if not root.is_dir(): raise FileNotFoundError(f"sessions directory not found: {root}")
    files = sorted(root.rglob("*.jsonl")); warnings = 0; seen = set(); call_seen = set(); rows = []
    contexts = {}; repeats = Counter(); metadata = {}; session_cwds = {}
    completed_records = fallback_direct_records = unsupported_records = 0
    completed_ids = set()
    completed_sessions = set()
    counted_completed_ids = set()
    for path in files:
        session = path.stem
        try:
            with path.open(errors="replace") as handle:
                for line in handle:
                    try: rec = json.loads(line)
                    except (json.JSONDecodeError, UnicodeDecodeError): continue
                    payload = rec.get("payload", rec) if isinstance(rec, dict) else {}
                    if isinstance(payload, dict) and rec.get("type") == "session_meta":
                        session = payload.get("id") or session
                    item = payload.get("item") if isinstance(payload, dict) and payload.get("type") == "item_completed" else None
                    if isinstance(item, dict) and item.get("type") == "CommandExecution" and item.get("id"):
                        completed_ids.add(item["id"])
                        completed_sessions.add(payload.get("thread_id") or session)
        except OSError: continue
    for path in files:
        fallback = path.stem
        try: handle = path.open(errors="replace")
        except OSError: warnings += 1; continue
        with handle:
            for line in handle:
                try: rec = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError): warnings += 1; continue
                if not isinstance(rec, dict): warnings += 1; continue
                typ = rec.get("type"); payload = rec.get("payload", rec)
                if not isinstance(payload, dict): continue
                if typ == "turn_context" or payload.get("type") == "turn_context":
                    ctx = payload.get("turn_context", payload)
                    key = rec.get("session_id") or rec.get("sessionId") or fallback
                    contexts[key] = (ctx.get("model") or ctx.get("model_id"), ctx.get("effort") or ctx.get("reasoning_effort"))
                    session_cwds[key] = ctx.get("cwd") or ctx.get("working_directory") or session_cwds.get(key)
                    metadata.setdefault(key, {k: ctx.get(k) for k in ("parent_thread_id", "source") if ctx.get(k) is not None})
                if typ == "session_meta":
                    key = rec.get("session_id") or rec.get("sessionId") or payload.get("id") or fallback
                    fallback = key
                    session_cwds[key] = payload.get("cwd") or session_cwds.get(key)
                    metadata.setdefault(key, {})
                    for k in ("source", "parent_thread_id"):
                        if payload.get(k) is not None: metadata[key][k] = payload[k]
                if typ == "response_item" or payload.get("type") == "function_call":
                    name = payload.get("name")
                    if name and name.split('.')[-1] in ("exec_command", "Bash"):
                        args = payload.get("arguments", {})
                        if isinstance(args, str):
                            try: args = json.loads(args)
                            except ValueError: args = {}
                        if isinstance(args, dict):
                            key = rec.get("session_id") or rec.get("sessionId") or fallback
                            if key in completed_sessions: continue
                            call_id = payload.get("call_id") or payload.get("callId")
                            if call_id in completed_ids: continue
                            dedup_id = (key, call_id) if call_id else None
                            if dedup_id and dedup_id in call_seen: continue
                            if dedup_id: call_seen.add(dedup_id)
                            command = args.get("cmd") or args.get("command") or ""
                            cwd = _resolved(args.get("workdir") or args.get("cwd") or session_cwds.get(key) or "")
                            if _in_repo(cwd, repo) and _in_bounds(rec.get("timestamp"), since, until):
                                fallback_direct_records += 1
                                if call_id: call_seen.add((key, call_id))
                                repeats[(key, cwd, hashlib.sha256(command.encode()).hexdigest(), _label(command))] += 1
                    elif name and name.split('.')[-1] == "exec":
                        key = rec.get("session_id") or fallback
                        if key not in completed_sessions and _in_bounds(rec.get("timestamp"), since, until) and _in_repo(_resolved(session_cwds.get(key)), repo):
                            unsupported_records += 1
                usage = payload.get("usage") if typ == "token_usage_record" else None
                if not isinstance(usage, dict): continue
                rid = rec.get("response_id") or rec.get("responseId") or payload.get("response_id") or payload.get("responseId")
                if not rid: warnings += 1
                if rid and rid in seen: continue
                if rid: seen.add(rid)
                key = rec.get("session_id") or rec.get("sessionId") or fallback
                if repo:
                    try:
                        if not session_cwds.get(key) or not pathlib.Path(session_cwds[key]).resolve().is_relative_to(pathlib.Path(repo).resolve()): continue
                    except (OSError, ValueError): continue
                model, effort = contexts.get(key, (None, None))
                model = model or payload.get("model") or "unknown"
                rows.append({"session": key, "task": rec.get("task_id") or rec.get("taskId") or payload.get("thread_id") or rec.get("thread_id") or rec.get("threadId") or "unknown", "model": model, "effort": effort or "unknown", "input": _num(usage, "input_tokens", "input"), "cached": _num(usage, "cached_input_tokens", "cache_read_input_tokens", "cached"), "output": _num(usage, "output_tokens", "output"), "reasoning": _num(usage, "reasoning_output_tokens", "reasoning_tokens", "reasoning"), "timestamp": rec.get("timestamp")})
    rows = [r for r in rows if _in_bounds(r["timestamp"], since, until)]
    for path in files:
        file_session = path.stem
        try: handle = path.open(errors="replace")
        except OSError: warnings += 1; continue
        with handle:
            for line in handle:
                try: rec = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError): continue
                if not isinstance(rec, dict): continue
                payload = rec.get("payload", rec)
                if isinstance(payload, dict) and rec.get("type") == "session_meta":
                    file_session = payload.get("id") or file_session
                item = payload.get("item") if isinstance(payload, dict) and payload.get("type") == "item_completed" else None
                if not isinstance(item, dict) or item.get("type") != "CommandExecution": continue
                session = rec.get("session_id") or rec.get("sessionId") or payload.get("thread_id") or file_session
                command = _command_text(item.get("command"))
                cwd = _resolved(item.get("cwd"))
                item_id = item.get("id")
                if item_id and item_id in counted_completed_ids: continue
                if not command or not _in_repo(cwd, repo) or not _in_bounds(rec.get("timestamp"), since, until): continue
                if item_id: counted_completed_ids.add(item_id)
                completed_records += 1
                repeats[(session, cwd, hashlib.sha256(command.encode()).hexdigest(), _label(command))] += 1
    totals = {k: sum(r[k] for r in rows) for k in ("input", "cached", "output", "reasoning")}
    by_model = defaultdict(lambda: {k: 0 for k in ("responses", "input", "cached", "output", "reasoning")})
    by_task = defaultdict(Counter)
    by_effort = defaultdict(Counter)
    for r in rows:
        b = by_model[r["model"]]; b["responses"] += 1
        for k in ("input", "cached", "output", "reasoning"): b[k] += r[k]
        for bucket in (by_task[r["task"]], by_effort[r["model"] + "/" + r["effort"]]):
            bucket["responses"] += 1
            for k in ("input", "cached", "output", "reasoning"): bucket[k] += r[k]
    selected_sessions = {r["session"] for r in rows}
    return {"totals": totals, "responses": len(rows), "by_model": dict(by_model), "by_task": dict(by_task), "by_effort": dict(by_effort), "sessions": {k: v for k, v in metadata.items() if k in selected_sessions}, "repeated_commands": [{"session": k[0], "cwd": k[1], "command_hash": k[2], "executable": k[3], "count": v} for k,v in repeats.items() if v > 1], "command_coverage": {"completed_records": completed_records, "fallback_direct_records": fallback_direct_records, "unsupported_orchestration_records": unsupported_records}, "warnings": warnings, "files": len(files), "limitations": ["Local token records only; not exact subscription cost. Cached input and reasoning are subsets of input and output respectively.", "Completed command records take precedence per session; otherwise direct-call attempts are counted. Repetition does not establish failure or waste; unfinished calls in sessions with completion records are omitted.", "Records without response IDs cannot be reliably deduplicated and increment warnings."]}

def main(argv=None):
    p=argparse.ArgumentParser(); p.add_argument("--sessions-dir", default="~/.codex/sessions"); p.add_argument("--since"); p.add_argument("--until"); p.add_argument("--repo"); p.add_argument("--json", action="store_true"); a=p.parse_args(argv)
    try:
        since = _time(a.since) if a.since else None
        if a.since and since is None: raise ValueError("invalid --since ISO date/time")
        until = _time(a.until) if a.until else None
        if a.until and until is None: raise ValueError("invalid --until ISO date/time")
        if since and until and since >= until: raise ValueError("--until must be after --since")
        result=scan(a.sessions_dir, since, a.repo, until=until)
    except (FileNotFoundError, ValueError) as e: print(str(e), file=sys.stderr); return 2
    if a.json: print(json.dumps(result, sort_keys=True))
    else:
        print(f"responses: {result['responses']}  input: {result['totals']['input']}  cached: {result['totals']['cached']}  output: {result['totals']['output']}  reasoning: {result['totals']['reasoning']}  warnings: {result['warnings']}")
        for model, values in sorted(result['by_model'].items()):
            print(f"{model}: {values['responses']} responses, {values['output']} output tokens")
        for note in result['limitations']: print(note)
    return 0
if __name__ == "__main__": sys.exit(main())
