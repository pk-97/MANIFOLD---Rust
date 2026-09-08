#!/usr/bin/env python3
"""Codex-only workflow checks. Hooks are guardrails, not a shell sandbox.

Reuses CC's read-only path/git inspection helpers, never its permission allows.
Rules are model-independent; no worker registration is required.
Arbitrary scripts/MCP tools and interactive stdin are outside enforcement.
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


def permit_path():
    """Return project-scoped permit storage independent of hook session aliases."""
    key = hashlib.sha256(str(ROOT).encode()).hexdigest()
    directory = Path(tempfile.gettempdir()) / f"manifold-codex-{os.getuid()}"
    directory.mkdir(mode=0o700, exist_ok=True)
    return directory / (key + ".permits.json")


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
    for tokens in segments(command):
        exe = Path(tokens[0]).name
        if exe == "land_branch.py" or (exe in {"python3", "python"} and len(tokens) > 1
                                        and Path(tokens[1]).name == "land_branch.py"):
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
            selected = args[args.index("--") + 1:]
            if not selected or any(a in {".", ":/"} or any(c in a for c in "*?[") for a in selected):
                return "Commit needs an exact nonempty path list."
            if Path(target).resolve() == ROOT:
                if any(not (p == "AGENTS.md" or p.startswith(".codex/") or
                            (p.startswith("docs/") and p.endswith(".md"))) for p in selected):
                    return "App changes must be committed in their slot worktree."
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
    command = args.get("command", args.get("cmd", ""))
    if not isinstance(command, str):
        return "Unrecognized tool input; no guard decision is safe."
    cwd = args.get("workdir") or event.get("cwd") or str(ROOT)
    if tool == "apply_patch":
        paths_guard = load("cc_paths", ROOT / ".claude/hooks/worktree-guard.py")
        return check_patch(event, command, cwd, paths_guard)
    if tool in {"Bash", "exec_command"}:
        shell_guard = load("cc_shell", ROOT / ".claude/hooks/preToolUseBash.py")
        missing_exec_workdir = tool == "exec_command" and not args.get("workdir")
        return (check_shell(event, command, cwd, shell_guard)
                or check_budget(event, command, cwd, missing_exec_workdir))
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
        if "gpu_proofs_gate.py" in names:
            yield "focused"
        elif any(re.search(r"(?:render|snapshot|rt_matrix|gpu_proofs|ui_flows).*\.py$", n) for n in names):
            yield "broad"


def consume_permit(command, cwd, allow_cwd_fallback=False):
    path = permit_path()
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = json.loads(path.read_text()) if path.exists() else {}
        key = budget_key(command, cwd)
        permit = data.get(key)
        if permit is None and allow_cwd_fallback:
            # Codex desktop currently omits exec_command's requested workdir
            # from some hook events. Fall back only when one live permit has
            # the exact command; multiple worktrees with the same command are
            # intentionally ambiguous and remain denied.
            matches = [candidate for candidate in data.values()
                       if candidate.get("command") == command.strip()
                       and candidate.get("remaining", 0) > 0
                       and time.time() < candidate.get("expires", 0)]
            permit = matches[0] if len(matches) == 1 else None
        if permit is None or permit.get("remaining", 0) <= 0 or time.time() >= permit.get("expires", 0):
            return False
        permit["remaining"] -= 1
        path.write_text(json.dumps(data))
        return True


def check_budget(event, command, cwd, allow_cwd_fallback=False):
    kinds = list(expensive_checks(command))
    if not kinds:
        return None
    path = state_path(event).with_suffix(".budget.json")
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = json.loads(path.read_text()) if path.exists() else {}
        key = budget_key(command, cwd)
        record = data.setdefault(key, {"attempts": 0})
        if ("broad" in kinds or record["attempts"] >= 2) and not consume_permit(
                command, cwd, allow_cwd_fallback):
            return ("Execution budget stopped this check. Broad/visual probes need a named, bounded exception; "
                    "focused commands get two attempts per session. Report evidence instead of looping. "
                    "The lead may use guard.py permit-check with the exact command, workdir and reason; "
                    "do not renew without changed code, new evidence, or explicit user direction. "
                    "Required checks inside land_branch.py/landing_gate.py remain unchanged.")
        record["attempts"] += 1
        path.write_text(json.dumps(data))
    return None


def permit_check(session_id, command, worktree, reason, attempts):
    if not command.strip() or not reason.strip() or not 1 <= attempts <= 3:
        raise ValueError("A check exception needs an exact command, reason and 1–3 attempts.")
    path = permit_path()
    with path.with_suffix(".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = json.loads(path.read_text()) if path.exists() else {}
        data[budget_key(command, worktree)] = {
            "reason": reason, "remaining": attempts, "expires": time.time() + 1800,
            "requested_by": session_id or "unknown", "command": command.strip(),
            "worktree": str(Path(worktree).resolve()),
        }
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
        parser.add_argument("action", choices=["permit-check"])
        parser.add_argument("--worktree", required=True)
        parser.add_argument("--command")
        parser.add_argument("--reason")
        parser.add_argument("--attempts", type=int, default=1)
        args = parser.parse_args()
        if not args.command or not args.reason:
            parser.error("permit-check requires --command and --reason")
        permit_check(os.environ.get("CODEX_THREAD_ID"), args.command, args.worktree, args.reason, args.attempts)
        print("Prepared bounded check exception; expires in 30 minutes.")
    else:
        main()
