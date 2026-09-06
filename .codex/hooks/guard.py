#!/usr/bin/env python3
"""Codex-only workflow checks. Hooks are guardrails, not a shell sandbox.

Reuses CC's read-only path/git inspection helpers, never its permission allows.
One Luna lane per session: its brief supplies an exact writable file list.
Arbitrary scripts/MCP tools and interactive stdin are outside scope enforcement.
"""
import hashlib
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
LUNA = "gpt-5.6-luna"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(cwd, *args):
    return subprocess.check_output(["git", "-C", str(cwd), *args], text=True,
                                   stderr=subprocess.DEVNULL, timeout=5).strip()


def state_path(event):
    key = hashlib.sha256((str(ROOT) + event["session_id"]).encode()).hexdigest()
    directory = Path(tempfile.gettempdir()) / f"manifold-codex-{os.getuid()}"
    directory.mkdir(mode=0o700, exist_ok=True)
    return directory / (key + ".json")


def scope_from_brief(message):
    match = re.search(r"^MANIFOLD_SCOPE: (.+)$", message, re.MULTILINE)
    if not match:
        raise ValueError('Lane brief needs MANIFOLD_SCOPE: {"worktree":"absolute path","files":["relative/file"]}. Use [] for read-only.')
    scope = json.loads(match[1])
    worktree = Path(scope["worktree"])
    if not worktree.is_absolute() or not worktree.is_dir():
        raise ValueError("Scope worktree must be an existing absolute directory.")
    worktree = worktree.resolve()
    files = scope["files"]
    if not isinstance(files, list) or not all(isinstance(p, str) for p in files):
        raise ValueError("Scope files must be a list of exact relative paths.")
    if files:
        pool = ROOT / ".claude/worktrees"
        if worktree.parent != pool or not re.fullmatch(r"slot-\d+", worktree.name):
            raise ValueError("Write lanes require an acquired slot-ring worktree.")
        if not (worktree / ".worktree-lease.json").is_file():
            raise ValueError("Acquire the slot before launching a write lane.")
        if Path(git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree:
            raise ValueError("Scope must name the worktree root.")
    for name in files:
        path = Path(name)
        if path.is_absolute() or ".." in path.parts or any(c in name for c in "*?["):
            raise ValueError("Scope files must be exact relative paths, without traversal or globs.")
        if not (worktree / path).resolve().is_relative_to(worktree):
            raise ValueError("Scope file escapes its worktree through a symlink.")
        if path.parts and path.parts[0] in {".git", ".claude", ".codex", "CLAUDE.md", "AGENTS.md"}:
            raise ValueError("Mechanical lanes cannot change harness configuration.")
    return {"worktree": str(worktree), "files": files}


def prepare_lane(session_id, task_name, worktree, files):
    if not session_id or not re.fullmatch(r"[a-z0-9_]+", task_name):
        raise ValueError("Lane preparation requires a session ID and a lowercase task name.")
    scope = scope_from_brief("MANIFOLD_SCOPE: " + json.dumps(
        {"worktree": worktree, "files": files}))
    path = state_path({"session_id": session_id}).with_suffix(".pending.json")
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps({"task_name": task_name, "scope": scope,
                               "created_at": time.time()}))
    tmp.replace(path)


def dispatch_scope(event, args, tool):
    # Native desktop hook events encrypt message; task_name remains plaintext.
    # Bind the lead's explicit, one-shot preparation to this session and task.
    if tool == "collaborationspawn_agent":
        path = state_path(event).with_suffix(".pending.json")
        if not path.is_file():
            raise ValueError("Prepare this lane with .codex/hooks/guard.py prepare-lane before native dispatch.")
        pending = json.loads(path.read_text())
        if pending["task_name"] != args.get("task_name"):
            raise ValueError("Prepared lane task name does not match native dispatch.")
        if not 0 <= time.time() - pending["created_at"] <= 600:
            raise ValueError("Prepared lane scope expired; prepare it again before dispatch.")
        scope = scope_from_brief("MANIFOLD_SCOPE: " + json.dumps(pending["scope"]))
        return scope, path
    return scope_from_brief(args.get("message", args.get("prompt", ""))), None


def patch_paths(command, cwd):
    paths = []
    for line in command.splitlines():
        match = re.match(r"^\*\*\* (?:Add File|Update File|Delete File|Move to): (.+)$", line)
        if match:
            paths.append((Path(cwd) / match[1]).resolve())
    if not paths:
        raise ValueError("Unrecognized patch paths; use the native apply_patch format.")
    return paths


def check_patch(event, command, cwd, paths_guard):
    paths = patch_paths(command, cwd)
    if event.get("model") == LUNA:
        scope = json.loads(state_path(event).read_text())
        allowed = {(Path(scope["worktree"]) / p).resolve() for p in scope["files"]}
        if any(not p.is_relative_to(Path(scope["worktree"])) for p in allowed):
            return "Lane scope now escapes its worktree through a symlink."
        if any(p not in allowed for p in paths):
            return "Lane edit is outside its exact file scope. Return the proposed scope change to the lead."
    for path in paths:
        # Slot contents are app work, not a licence to change CC tooling.
        if path.name == "CLAUDE.md" or (path.is_relative_to(ROOT / ".claude")
                and not path.is_relative_to(ROOT / ".claude/worktrees")):
            return "CC configuration is outside this Codex workflow."
        pool = ROOT / ".claude/worktrees"
        if path.is_relative_to(pool):
            parts = path.relative_to(pool).parts
            if len(parts) > 1 and parts[1] == ".claude":
                return "CC configuration is outside this Codex workflow."
        if paths_guard.in_main_checkout(path):
            if path == ROOT / "AGENTS.md" or path.is_relative_to(ROOT / ".codex"):
                continue
            if paths_guard.is_doc_fast_path(path) or path in paths_guard.merge_conflict_paths():
                continue
            return "App edits belong in an acquired slot worktree; keep main runnable."
    return None


def segments(command):
    lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|<>()\n")
    lexer.whitespace = " \t\r"
    lexer.whitespace_split = True
    lexer.commenters = "#"
    current = []
    for token in lexer:
        if token and all(c in ";&|<>()\n" for c in token):
            if current:
                yield current
            current = []
        else:
            current.append(token)
    if current:
        yield current


def check_shell(event, command, cwd, shell_guard):
    # Reuse established detection; a Codex hook never returns CC's allow/ask.
    for check in (shell_guard.worktree_add_guard, shell_guard.destructive_outward_guard):
        if check(command, cwd):
            return "Destructive/outward git action or raw worktree operation blocked. Use the slot ring and normal landing workflow."
    worker = event.get("model") == LUNA
    if worker:
        scope = json.loads(state_path(event).read_text())
        # Shell programs are not parsed for file writes. Keep worker execution
        # to read tools and established validation commands; edits use patches.
        if re.search(r"[<>`]|\$\(|\b(?:eval|exec|xargs)\b", command):
            return "Lane shell redirection/substitution is outside the scope guard; use native patches and simple checks."
    for tokens in segments(command):
        exe = Path(tokens[0]).name
        if worker and exe != "git":
            reads = {"rg", "ls", "cat", "head", "tail", "wc", "pwd", "stat"}
            checks = {"cargo", "gpu_proofs_gate.py"}
            if exe not in reads | checks:
                return "Lane shell is limited to read tools and cargo/GPU checks. Use apply_patch for scoped edits."
            if exe == "cargo" and (len(tokens) < 2 or tokens[1] not in {"check", "clippy", "test", "nextest", "build"}):
                return "Lane cargo commands are limited to build and validation."
            if exe in checks and (not scope["files"] or Path(cwd).resolve() != Path(scope["worktree"])):
                return "Run lane validation from its assigned writable worktree."
        if exe == "land_branch.py" or (exe in {"python3", "python"} and len(tokens) > 1
                                        and Path(tokens[1]).name == "land_branch.py"):
            if worker:
                return "Only the lead may land. Return the diff and check results."
            if any(t.startswith("--named-red") for t in tokens):
                return "Codex landing requires a green gate; report failures to Peter."
        if exe != "git":
            continue
        target, sub, args = shell_guard._git_checkout_dir(["git", *tokens[1:]], cwd)
        if target is None or sub is None:
            return "Unrecognized git invocation; use a single explicit git command."
        if sub in {"reset", "clean", "rebase", "cherry-pick", "restore"}:
            return "History/tree rewrite blocked; preserve the shared work and merge lineage."
        if sub == "branch" and any(a in {"-D", "-f", "--force"} for a in args):
            return "Forced branch rewrites/deletions are outside normal delivery."
        if sub == "add" and ("--" not in args or any(a in {".", "-A", "--all", "-u", ":/"} for a in args)):
            return "Stage exact paths with git add -- <paths>."
        if sub == "commit":
            if "--" not in args or any(a in {"-a", "--all", "--amend"} for a in args):
                return "Commit exact paths with git commit -m '...' -- <paths>."
            if worker:
                return "Luna returns edits and test results; the lead reviews and commits."
            selected = args[args.index("--") + 1:]
            if not selected or any(a in {".", ":/"} or any(c in a for c in "*?[") for a in selected):
                return "Commit needs an exact nonempty path list."
            if Path(target).resolve() == ROOT:
                if any(not (p == "AGENTS.md" or p.startswith(".codex/") or
                            (p.startswith("docs/") and p.endswith(".md"))) for p in selected):
                    return "App changes must be committed in their slot worktree."
        if worker and sub not in {"status", "diff", "log", "show", "rev-parse", "ls-files", "grep", "blame", "merge-base"}:
            return "Lane git access is read-only; the lead owns commits and landing."
        if sub == "merge" and Path(target).resolve() == ROOT:
            if not any(a in {"--abort", "--continue"} for a in args):
                return "Land through scripts/land_branch.py so the existing validation gate runs before merge/push."
        if sub == "push":
            if Path(target).resolve() != ROOT or args not in (["origin", "main"],):
                return "Raw push is limited to origin main for documentation/Codex-only commits; app delivery uses land_branch.py."
            changed = git(ROOT, "diff", "--name-only", "origin/main..HEAD").splitlines()
            if any(not (p == "AGENTS.md" or p.startswith(".codex/") or
                        (p.startswith("docs/") and p.endswith(".md"))) for p in changed):
                return "App delivery uses scripts/land_branch.py; raw push does not establish a passed gate."
    return None


def evaluate(event):
    tool = event.get("tool_name", "").split(".")[-1]
    args = event.get("tool_input") or {}
    if tool in {"spawn_agent", "collaborationspawn_agent", "Agent"}:
        if event.get("model") == LUNA:
            return "Mechanical lanes cannot delegate."
        if not args.get("model"):
            return "Specify the worker model explicitly; mechanical work uses gpt-5.6-luna."
        if args["model"] != LUNA:
            return "This mechanical-lane configuration permits Luna only. Discuss a separate consult with Peter."
        if args.get("reasoning_effort") != "low":
            return "Mechanical Luna lanes require explicit reasoning_effort: low."
        scope, pending_path = dispatch_scope(event, args, tool)
        path = state_path(event)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(scope))
        tmp.replace(path)
        if pending_path is not None:
            pending_path.unlink()
        return None
    command = args.get("command", args.get("cmd", ""))
    if not isinstance(command, str):
        return "Unrecognized tool input; no guard decision is safe."
    cwd = args.get("workdir") or event.get("cwd") or str(ROOT)
    if tool == "apply_patch":
        paths_guard = load("cc_paths", ROOT / ".claude/hooks/worktree-guard.py")
        return check_patch(event, command, cwd, paths_guard)
    if tool in {"Bash", "exec_command"}:
        shell_guard = load("cc_shell", ROOT / ".claude/hooks/preToolUseBash.py")
        return check_shell(event, command, cwd, shell_guard) or check_budget(event, command, cwd)
    return None


def budget_key(command, cwd):
    return hashlib.sha256((str(Path(cwd).resolve()) + "\n" + command.strip()).encode()).hexdigest()


def expensive_checks(command):
    """Recognize direct commands and common build-lock/env wrappers, not scripts."""
    for tokens in segments(command):
        names = [Path(t).name for t in tokens]
        if "cargo" in names:
            args = tokens[names.index("cargo") + 1:]
            if any(a in {"test", "nextest", "clippy", "check", "build"} for a in args):
                scoped = any(a in {"-p", "--package", "--manifest-path"} or
                             a.startswith(("--package=", "--manifest-path=", "-pmanifold")) for a in args)
                yield "broad" if "--workspace" in args or not scoped else "focused"
            if "perf-soak" in args:
                yield "broad"
        if set(names) & {"trunk_health.py", "feature_matrix.py", "launch_live_ui.py"}:
            yield "broad"
        if any(re.search(r"(?:render|snapshot|rt_matrix|gpu_proofs|ui_flows).*\.py$", n) for n in names):
            yield "broad"


def check_budget(event, command, cwd):
    kinds = list(expensive_checks(command))
    if not kinds:
        return None
    path = state_path(event).with_suffix(".budget.json")
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = json.loads(path.read_text()) if path.exists() else {}
        key = budget_key(command, cwd)
        record = data.setdefault(key, {"attempts": 0})
        permit = record.get("permit", {})
        if permit.get("remaining", 0) > 0 and time.time() < permit.get("expires", 0):
            permit["remaining"] -= 1
        elif "broad" in kinds or record["attempts"] >= 2:
            return ("Execution budget stopped this check. Broad/visual probes need a named, bounded exception; "
                    "focused commands get two attempts per session. Report evidence instead of looping. "
                    "The lead may use guard.py permit-check with the exact command, workdir and reason; "
                    "do not renew without changed code, new evidence, or explicit user direction. "
                    "Required checks inside land_branch.py/landing_gate.py remain unchanged.")
        record["attempts"] += 1
        path.write_text(json.dumps(data))
    return None


def permit_check(session_id, command, worktree, reason, attempts):
    if not session_id or not command.strip() or not reason.strip() or not 1 <= attempts <= 3:
        raise ValueError("A check exception needs session, exact command, reason and 1–3 attempts.")
    path = state_path({"session_id": session_id}).with_suffix(".budget.json")
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = json.loads(path.read_text()) if path.exists() else {}
        record = data.setdefault(budget_key(command, worktree), {"attempts": 0})
        record["permit"] = {"reason": reason, "remaining": attempts, "expires": time.time() + 1800}
        path.write_text(json.dumps(data))


def main():
    try:
        reason = evaluate(json.load(sys.stdin))
    except ValueError as exc:
        reason = str(exc)
    except (KeyError, OSError, TypeError, subprocess.SubprocessError) as exc:
        reason = f"Codex guard could not validate this action ({type(exc).__name__}); repair the input or hook before retrying."
    if reason:
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse",
              "permissionDecision": "deny", "permissionDecisionReason": reason}}))


if __name__ == "__main__":
    if len(sys.argv) > 1:
        import argparse
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("action", choices=["prepare-lane", "permit-check"])
        parser.add_argument("--task")
        parser.add_argument("--worktree", required=True)
        parser.add_argument("--files", nargs="*", default=[])
        parser.add_argument("--command")
        parser.add_argument("--reason")
        parser.add_argument("--attempts", type=int, default=1)
        args = parser.parse_args()
        if args.action == "permit-check":
            if not args.command or not args.reason:
                parser.error("permit-check requires --command and --reason")
            permit_check(os.environ.get("CODEX_THREAD_ID"), args.command, args.worktree, args.reason, args.attempts)
            print("Prepared bounded check exception; expires in 30 minutes.")
        else:
            if not args.task:
                parser.error("prepare-lane requires --task")
            prepare_lane(os.environ.get("CODEX_THREAD_ID"), args.task, args.worktree, args.files)
            print(f"Prepared {args.task}; dispatch within 10 minutes.")
    else:
        main()
