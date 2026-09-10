#!/usr/bin/env python3
"""Read Codex session jsonl files and report token usage (stdlib only)."""
import argparse, datetime as dt, hashlib, json, os, pathlib, sys
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

def scan(sessions_dir, since=None, repo=None):
    root = pathlib.Path(os.path.expanduser(sessions_dir))
    if not root.is_dir(): raise FileNotFoundError(f"sessions directory not found: {root}")
    files = sorted(root.rglob("*.jsonl")); warnings = 0; seen = set(); call_seen = set(); rows = []
    contexts = {}; repeats = Counter(); metadata = {}; session_cwds = {}
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
                            call_id = payload.get("call_id") or payload.get("callId")
                            if call_id and call_id in call_seen: continue
                            if call_id: call_seen.add(call_id)
                            command = args.get("cmd") or args.get("command") or ""
                            cwd = args.get("workdir") or args.get("cwd") or session_cwds.get(key) or ""
                            if (not repo or (cwd and pathlib.Path(cwd).resolve().is_relative_to(pathlib.Path(repo).resolve()))) and (not since or (_time(rec.get("timestamp")) or dt.datetime.min.replace(tzinfo=dt.timezone.utc)) >= since):
                                repeats[(key, cwd, hashlib.sha256(command.encode()).hexdigest()[:12], _label(command))] += 1
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
    if since: rows = [r for r in rows if (_time(r["timestamp"]) or dt.datetime.min.replace(tzinfo=dt.timezone.utc)) >= since]
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
    return {"totals": totals, "responses": len(rows), "by_model": dict(by_model), "by_task": dict(by_task), "by_effort": dict(by_effort), "sessions": {k: v for k, v in metadata.items() if k in selected_sessions}, "repeated_commands": [{"session": k[0], "cwd": k[1], "command_hash": k[2], "executable": k[3], "count": v} for k,v in repeats.items() if v > 1], "warnings": warnings, "files": len(files), "limitations": ["Local token records only; not exact subscription cost. Cached input and reasoning are subsets of input and output respectively.", "Repetitions are not necessarily waste. Only direct recorded shell calls are counted; calls inside functions.exec are not parsed.", "Records without response IDs cannot be reliably deduplicated and increment warnings."]}

def main(argv=None):
    p=argparse.ArgumentParser(); p.add_argument("--sessions-dir", default="~/.codex/sessions"); p.add_argument("--since"); p.add_argument("--repo"); p.add_argument("--json", action="store_true"); a=p.parse_args(argv)
    try:
        since = _time(a.since) if a.since else None
        if a.since and since is None: raise ValueError("invalid --since ISO date/time")
        result=scan(a.sessions_dir, since, a.repo)
    except (FileNotFoundError, ValueError) as e: print(str(e), file=sys.stderr); return 2
    if a.json: print(json.dumps(result, sort_keys=True))
    else:
        print(f"responses: {result['responses']}  input: {result['totals']['input']}  cached: {result['totals']['cached']}  output: {result['totals']['output']}  reasoning: {result['totals']['reasoning']}  warnings: {result['warnings']}")
        for model, values in sorted(result['by_model'].items()):
            print(f"{model}: {values['responses']} responses, {values['output']} output tokens")
        for note in result['limitations']: print(note)
    return 0
if __name__ == "__main__": sys.exit(main())
