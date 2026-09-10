"""Advisory subsystem context for supported patch and agent-dispatch events."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys


def context_for_event(event, root):
    tool = event.get("tool_name", "").split(".")[-1]
    args = event.get("tool_input") or {}
    if not isinstance(args, dict):
        return ""
    cwd = Path(args.get("workdir") or event.get("cwd") or root).resolve()
    paths = []
    if tool == "apply_patch":
        command = args.get("command", args.get("cmd", ""))
        paths = [(cwd / p).resolve() for p in re.findall(
            r"^\*\*\* (?:Add File|Update File|Delete File|Move to): (.+)$", command, re.M)]
    elif tool in {"spawn_agent", "collaborationspawn_agent", "Agent"}:
        message = args.get("message", args.get("prompt", ""))
        # Recognize explicit slot paths in a dispatch without interpreting prose as shell.
        slots = re.findall(re.escape(str(root)) + r"/\.claude/worktrees/slot-\d+", message)
        if len(set(slots)) == 1:
            cwd = Path(slots[0])
        paths = [cwd / p for p in re.findall(r"(?<![\w])(?:crates|docs|scripts)/[\w./-]+", message)]
    if not paths:
        return ""
    groups = {}
    for path in paths:
        path = path.resolve()
        pool = root / ".claude/worktrees"
        if path.is_relative_to(pool) and len(path.relative_to(pool).parts) > 1:
            repo = pool / path.relative_to(pool).parts[0]
        elif path.is_relative_to(root):
            repo = root
        else:
            continue
        groups.setdefault(repo, []).append(str(path))
    spec = importlib.util.spec_from_file_location("codex_prepare_context", root / "scripts/codex_prepare.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    chunks = []
    for repo, owned in groups.items():
        result = module.select_context(repo, owned)
        for subsystem in result["subsystems"]:
            chunks.append(f"{subsystem['name']}: {subsystem['invariants']} Reuse: "
                          + ", ".join(subsystem["entry_points"]) + ". Read: "
                          + ", ".join(subsystem["docs"]))
    if not chunks:
        return ""
    return "MANIFOLD subsystem guidance (advisory; include in worker brief when dispatching):\n" + "\n".join(dict.fromkeys(chunks))
