#!/usr/bin/env python3
"""Mechanical landing ceremony (GIT_TREE_DISCIPLINE.md section 2 (Landing protocol)).

One command after the lead's review passes: merges origin/main into the
branch (in its slot worktree), runs landing_gate.py, then under the landing
lock merges --no-ff to main in the main checkout, commits bead closes, and
pushes once, so a landing moves main once. Deletes the
branch when it is an ancestor of origin/main.

The landing lock is an flock in the git common dir, shared by every worktree.
It covers only: origin/main still equals the gate's base, merge, commits,
push. No gate ever runs under it. When main moved, the lock is released, the
branch takes the new main and is gated again outside it, then the lock is
retried. A waiter prints who holds it.

The JUDGMENT stays with the lead: the review, the named-red call (pass
--named-red BUG-id --reason "..."), the design-doc status edits. Required
steps exit on failure with the step named. Push happens only after a green
gate or an explicit named red over a gate that ran every check (exit
CHECKS_RED). After a moved-main re-gate the named red carries over only when
that gate also ran every check and went red on exactly the same check labels;
anything else stops. --named-red requests the complete gate run.
After verified landing, slot detachment, release and branch deletion are cleanup:
failures are notes, not landing failures. Unlanded branches are never deleted.
Before merging it records the gated tree as a land pass in the git common dir
(land-passes-v1/<tree>.json); the Bash hook denies a manual merge or push to
main of any tree without one (preToolUseBash.py land_pass_guard).
Before any fetch it refuses a slot that is not the named branch's tip or is
dirty, and
exits 0 with no row when the tip is already on origin/main.
Every attempt past argument parsing, refused and cancelled ones included,
appends one row to .claude/orchestration/landing-runs.jsonl in the main checkout.

Usage:
  scripts/land_branch.py <branch> --worktree <path> --message '<merge msg>' \
      [--named-red BUG-xxxx --reason '<why safe>'] \
      [--close-bead BUG-xxxx ...] [--close-reason '<closing note>'] \
      [--lead 'Claude lead']

Obsolete when: the landing protocol itself changes shape (edit both).
"""

import argparse
import contextlib
import fcntl
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
import time
from pathlib import Path

from gate_cancellation import Cancelled, cancellation_signals
from landing_gate import (CHECKS_RED, record_incomplete,
                          run_cmd, landing_log_path, stop_child)

MAIN = Path("/Users/peterkiemann/MANIFOLD - Rust")
LOCK_NAME = "manifold-landing.lock"
RUNS_LOG = ".claude/orchestration/landing-runs.jsonl"
LAND_PASS_DIR = "land-passes-v1"  # reader: land_pass_guard in .claude/hooks/preToolUseBash.py
ATTEMPT = None  # this attempt's landing-runs.jsonl row, set by main()

def step(name, cmd, cwd, check=True, cleanup=False):
    print(f"[land] {name}: {' '.join(cmd)}", flush=True)
    try:
        live = landing_log_path(cwd, 'land-' + name.replace(' ', '-').replace('/', '-'))
        print(f"[RUN] land/{name} (live transcript: {live})", flush=True)
        code, out, err, _ = run_cmd(cmd, cwd, timeout=3600, live_log=live)
    except (OSError, subprocess.SubprocessError) as error:
        if not cleanup:
            raise
        code, out, err = 1, '', str(error)
    r = subprocess.CompletedProcess(cmd, code, out, err)
    status = 'PASS' if code == 0 else ('NOTE' if cleanup else 'FAIL')
    print(f"[{status}] land/{name}", flush=True)
    if r.returncode != 0 and cleanup:
        print(f"[land] NOTE: {name} incomplete; landing is already pushed.", flush=True)
        for line in (out + '\n' + err).strip().splitlines():
            print(f"    {line}", flush=True)
    elif r.returncode != 0 and check:
        print(f"[land] FAILED at {name}:\n{r.stdout}\n{r.stderr}", file=sys.stderr)
        sys.exit(1)
    return r

def run_landing_gate(cmd, cwd, log_path):
    """Expose check progress immediately and preserve the complete gate log."""
    print(f"[land] landing_gate: {' '.join(cmd)}", flush=True)
    print(f"[land] complete landing gate transcript: {log_path}", flush=True)
    with log_path.open("w") as log:
        proc = subprocess.Popen(
            cmd, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1, start_new_session=True)
        proc._requires_cleanup = True
        try:
            for line in proc.stdout:
                log.write(line)
                log.flush()
                print(line, end="", flush=True)
            return proc.wait()
        except BaseException:
            stop_child(proc, graceful=True)
            log.write("[INCOMPLETE] landing gate: parent cancelled; children stopped\n")
            log.flush()
            raise
        finally:
            proc.stdout.close()

def git_out(name, cmd, cwd):
    return step(name, cmd, cwd).stdout.strip()

def red_labels(gate_text):
    """Check labels the gate summary printed as FAIL (never nextest's 'FAIL [')."""
    return frozenset(m[1] for m in re.finditer(r'^FAIL (?!\[)(\S+)', gate_text, re.M))

def gpu_wait_seconds(gate_text):
    waits = [float(m[1]) for m in re.finditer(r'^gpu-queue wait: ([0-9.]+)s', gate_text, re.M)]
    return sum(waits) if waits else None

def carry_over_allowed(named_reds, code, reds):
    """A named red covers a moved-main re-gate only when nothing new went red.

    The gate must have run every check (CHECKS_RED) and the red set must be
    exactly the one the lead reviewed: a vanished red is also a changed tree."""
    return bool(named_reds) and code == CHECKS_RED and reds == named_reds

def gate(gate_cmd, worktree, gate_log):
    """One gate run, counted in the attempt row. Returns (exit code, red labels)."""
    code = run_landing_gate(gate_cmd, worktree, gate_log)
    ATTEMPT['gate_runs'] += 1
    try:
        text = gate_log.read_text(errors='replace')
    except OSError:
        text = ''
    wait = gpu_wait_seconds(text)
    if wait is not None:
        ATTEMPT['gpu_wait_s'] = round((ATTEMPT['gpu_wait_s'] or 0) + wait, 1)
    return code, red_labels(text)

def lock_path():
    """In the git common dir, so every worktree of this repo shares one lock."""
    common = git_out('git common dir', ['git', 'rev-parse', '--git-common-dir'], MAIN)
    return (MAIN / common).resolve() / LOCK_NAME

@contextlib.contextmanager
def landing_lock(path, holder):
    """Exclusive flock around the main-moving critical section; yields seconds waited.

    The kernel drops a flock when its process dies, so a crash or cancel
    never strands the lock."""
    started = time.monotonic()
    with open(path, 'a+') as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            handle.seek(0)
            who = handle.read().strip() or 'an unnamed landing'
            print(f'[land] waiting for the landing lock held by {who}', flush=True)
            fcntl.flock(handle, fcntl.LOCK_EX)
        waited = time.monotonic() - started
        try:
            handle.truncate(0)
            handle.write(holder + '\n')
            handle.flush()
            yield waited
        finally:
            handle.truncate(0)
            handle.flush()
            fcntl.flock(handle, fcntl.LOCK_UN)

def record_land_pass(tree, commit, branch, named_red):
    """Record that `tree` passed the landing gate, in the git common dir next to
    gate-passes-v1. The bash hook denies a manual merge or push of any other
    tree to main. Written just before the merge, so a crash leaves a harmless
    record, never an unrecorded landing."""
    directory = lock_path().parent / LAND_PASS_DIR
    directory.mkdir(parents=True, exist_ok=True)
    record = {'schema': 1, 'tree': tree, 'commit': commit, 'branch': branch,
              'named_red': named_red or None,
              'time': datetime.now(timezone.utc).isoformat()}
    tmp = directory / f'.{tree}.tmp'
    tmp.write_text(json.dumps(record) + '\n')
    os.replace(tmp, directory / f'{tree}.json')

def stop(outcome, message):
    ATTEMPT['outcome'] = outcome
    print(f'[land] {message}', file=sys.stderr)
    sys.exit(1)

def land_gated(a, worktree, base, gated_commit, verdict):
    """The critical section, run under the landing lock. Returns the new
    origin/main when main moved since the gate's base, else None once pushed.

    Main moves once per landing: merge and bead closes go in one push."""
    step('fetch', ['git', 'fetch', 'origin', 'main'], MAIN)
    origin = git_out('origin/main', ['git', 'rev-parse', 'origin/main'], MAIN)
    if origin != base:
        return origin
    if git_out('local main', ['git', 'rev-parse', 'main'], MAIN) != origin:
        if step('fast-forward main', ['git', 'merge', '--ff-only', 'origin/main'], MAIN,
                check=False).returncode:
            stop('error', 'local main has commits origin/main lacks; push or drop them first.')
    gated = git_out('gated tree', ['git', 'rev-parse', f'{gated_commit}^{{tree}}'], worktree)
    record_land_pass(gated, gated_commit, a.branch, a.named_red if verdict else None)
    merged = step('merge --no-ff to main',
                  ['git', 'merge', '--no-ff', '--no-commit', gated_commit, '-m', a.message], MAIN,
                  check=False)
    if merged.returncode:
        step('abort failed landing merge', ['git', 'merge', '--abort'], MAIN, check=False)
        stop('conflict', 'main merge conflicted; resolve current main in the slot and retry.')
    if git_out('verify merge tree', ['git', 'write-tree'], MAIN) != gated:
        step('abort changed merge tree', ['git', 'merge', '--abort'], MAIN, check=False)
        stop('error', 'merge tree differs from the gated tree although main did not move.')
    if step('pending merge', ['git', 'rev-parse', '-q', '--verify', 'MERGE_HEAD'],
            MAIN, check=False).returncode == 0:
        step('commit gated merge', ['git', 'commit', '--no-edit'], MAIN)
    for bead in a.close_bead:
        step(f"close {bead}", ["bd", "close", bead, "-r", f"{a.close_reason} {a.lead}"], MAIN)
    if a.close_bead:
        step("commit beads", ["git", "add", "--", ".beads/issues.jsonl"], MAIN)
        step("commit beads", ["git", "commit", "-m",
                              f"beads: {', '.join(a.close_bead)} closed with the {a.branch} landing. {a.lead}",
                              "--", ".beads/issues.jsonl"], MAIN)
    step("push main", ["git", "push", "origin", "main"], MAIN)
    return None

def regate_moved_main(worktree, gate_cmd, gate_log):
    """Outside the lock: bring the branch onto the new origin/main and gate it."""
    step('merge moved main into branch', ['git', 'merge', 'origin/main', '--no-edit'], worktree)
    gated_commit = git_out('pin gate commit', ['git', 'rev-parse', 'HEAD'], worktree)
    ATTEMPT['moved_main_regates'] += 1
    code, reds = gate(gate_cmd, worktree, gate_log)
    return gated_commit, code, reds

def new_attempt(branch):
    return {'branch': branch, 'start': datetime.now(timezone.utc).isoformat(), 'end': None,
            'outcome': None, 'gate_runs': 0, 'moved_main_regates': 0,
            'lock_wait_s': 0.0, 'gpu_wait_s': None}

def write_attempt(attempt):
    attempt['end'] = datetime.now(timezone.utc).isoformat()
    attempt['lock_wait_s'] = round(attempt['lock_wait_s'], 1)
    try:
        path = MAIN / RUNS_LOG
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, 'a') as log:
            log.write(json.dumps(attempt) + '\n')
    except OSError as error:
        print(f'[land] WARN: landing log not written: {error}', file=sys.stderr)

class Refused(Exception):
    """A start check failed; the message names the reason and the rerun."""

def probe(args, cwd):
    """Read-only git query for the start checks: (exit code, stripped stdout)."""
    r = subprocess.run(['git', *args], cwd=str(cwd), capture_output=True, text=True)
    return r.returncode, r.stdout.strip()

def start_checks(branch, wt):
    """Before any fetch or gate: is this slot really the named branch, and clean?

    Returns True when the branch's own commits are already on origin/main,
    checked only after the slot proves clean. Raises Refused
    otherwise-unsafe. A lease or a live holder pid is never evidence either
    way: a subagent's holder is its lead's process."""
    q = f'git -C "{wt}"'
    if not wt.exists():
        raise Refused(f'worktree {wt} missing. Check: ls "{wt}"')
    code, tip = probe(['rev-parse', '--verify', '-q', f'refs/heads/{branch}^{{commit}}'], wt)
    if code or not tip:
        raise Refused(f'branch {branch} does not exist. Check: {q} rev-parse --verify refs/heads/{branch}')
    code, head = probe(['rev-parse', 'HEAD'], wt)
    if code or head != tip:
        raise Refused(f'worktree HEAD {head[:9] or "?"} is not the tip {tip[:9]} of {branch}; '
                      f'the slot holds other work. Check: {q} rev-parse HEAD {branch}')
    code, dirty = probe(['status', '--porcelain'], wt)
    if code or dirty:
        raise Refused(f'worktree is not clean; commit or remove the changes. Check: {q} status --short')
    if probe(['merge-base', '--is-ancestor', tip, 'origin/main'], wt)[0] != 0:
        return False
    # On main's first-parent line means the branch never had commits of its own;
    # a landed branch's tip arrives through a merge's second parent.
    code, trunk = probe(['rev-list', '--first-parent', 'origin/main'], wt)
    if code or tip in trunk.split():
        raise Refused(f'nothing to land: {branch} has no commits beyond origin/main. '
                      f'Check: {q} log --oneline origin/main..{branch}')
    return True

def parse_args(argv):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("branch")
    p.add_argument("--worktree", required=True)
    p.add_argument("--message", required=True)
    p.add_argument("--named-red")
    p.add_argument("--reason")
    p.add_argument("--skip-gpu", metavar="REASON",
                   help="defer GPU proofs with a recorded reason; all other gates must pass")
    p.add_argument("--close-bead", action="append", default=[])
    p.add_argument("--close-reason", default="")
    p.add_argument("--lead", default="Claude lead")
    return p.parse_args(argv)

def main():
    global ATTEMPT
    # --help and argument errors exit here, before any landing-runs row exists.
    a = parse_args(sys.argv[1:])
    assert a.branch != "main", "land a branch, never main itself"
    wt = Path(a.worktree)
    ATTEMPT = new_attempt(a.branch)
    try:
        landed = start_checks(a.branch, wt)
    except Refused as why:
        ATTEMPT['outcome'] = 'refused'
        write_attempt(ATTEMPT)
        print(f'[land] REFUSED: {why}', file=sys.stderr)
        return 1
    except Exception as error:  # fail closed: a broken check never lets a landing start
        ATTEMPT['outcome'] = 'error'
        write_attempt(ATTEMPT)
        print(f'[land] REFUSED: start checks failed ({type(error).__name__}: {error}). '
              f'Fix the check, then rerun the land command.', file=sys.stderr)
        return 1
    if landed:
        print(f'[land] already landed: {a.branch} tip is on origin/main; nothing to do.')
        return 0
    try:
        with cancellation_signals():
            result = _main(a)
        ATTEMPT['outcome'] = 'landed'
        return result
    except Cancelled as error:
        ATTEMPT['outcome'] = 'cancelled'
        return record_incomplete(wt, error)
    except SystemExit:
        ATTEMPT['outcome'] = ATTEMPT['outcome'] or 'error'
        raise
    except BaseException:
        ATTEMPT['outcome'] = 'error'
        raise
    finally:
        write_attempt(ATTEMPT)

def _main(a):
    wt = Path(a.worktree)

    step("fetch", ["git", "fetch", "origin", "main"], MAIN)
    base = git_out("origin/main", ["git", "rev-parse", "origin/main"], MAIN)
    step("merge origin/main into branch", ["git", "merge", base, "--no-edit"], wt)

    # A reviewed named red needs every leg's result, including when the known
    # red is cheap. Completion is still enforced by CHECKS_RED below.
    gate_cmd = [sys.executable, "-u", "scripts/landing_gate.py", "--repo", str(wt.resolve())]
    if a.named_red:
        gate_cmd.append("--keep-going")
    if a.skip_gpu:
        gate_cmd += ["--skip-gpu", a.skip_gpu]
    log_dir = wt / "target" / "landing-logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    gate_log = (log_dir / f"landing-gate-{stamp}-{time.time_ns()}.log").resolve()
    gated_commit = git_out('pin gate commit', ['git', 'rev-parse', 'HEAD'], wt)
    gate_returncode, named_reds = gate(gate_cmd, wt, gate_log)
    if gate_returncode != 0:
        # A recorded --skip-gpu is a reviewed scope call, not missing coverage:
        # the gate still exits CHECKS_RED only when every other check ran.
        if not (a.named_red and a.reason):
            stop('red', "gate red and no --named-red/--reason given — stopping. "
                 "Review the failure; land over it only with an explicit named red.")
        if gate_returncode != CHECKS_RED:
            stop('red', f"the gate exited {gate_returncode} without running every check (a refusal, "
                 "a crash or a moved tree); a named red covers only checks that ran red — stopping.")
    else:
        named_reds = frozenset()

    holder = f"{a.branch} (pid {os.getpid()}, {a.lead}, since {{}})"
    while True:
        with landing_lock(lock_path(), holder.format(datetime.now().strftime('%H:%M:%S'))) as waited:
            ATTEMPT['lock_wait_s'] += waited
            moved = land_gated(a, wt, base, gated_commit, verdict=bool(named_reds))
        if moved is None:
            break
        print(f"[land] main moved to {moved[:9]} since the gate's base; re-gating outside the lock.",
              flush=True)
        base = moved
        gated_commit, code, reds = regate_moved_main(wt, gate_cmd, gate_log)
        if code == 0:
            named_reds = frozenset()
        elif not carry_over_allowed(named_reds, code, reds):
            new = ', '.join(sorted(reds - named_reds)) or 'none'
            stop('red', f"moved-main gate red (exit {code}; new reds: {new}); the named red covered "
                 f"{', '.join(sorted(named_reds)) or 'nothing'}. Stopping before main changes.")
        else:
            print(f"[land] moved-main gate red on exactly the named reds "
                  f"({', '.join(sorted(reds))}); the named red carries over.", flush=True)

    anc = step("verify landed ancestry", ["git", "merge-base", "--is-ancestor", a.branch, "origin/main"],
               MAIN, check=False, cleanup=True).returncode == 0
    if anc:
        if wt.resolve().parent == (MAIN / ".claude/worktrees").resolve():
            # Detach before releasing ownership: acquire may immediately reuse
            # the slot once the lease is gone. Never detach an unlanded tip.
            detached = step("detach landed slot", ["git", "switch", "--detach"], wt,
                            cleanup=True)
            if detached.returncode == 0:
                step("release landed slot", [sys.executable, str(MAIN / "scripts/agent-worktree.py"),
                                             "release", wt.name], MAIN, cleanup=True)
        step("delete branch", ["git", "branch", "-d", a.branch], MAIN, cleanup=True)
    else:
        print(f"[land] NOTE: {a.branch} tip is not an ancestor of origin/main — left undeleted.")

    print(f"[land] DONE: {a.branch} landed. {a.lead}", flush=True)

if __name__ == "__main__":
    sys.exit(main())
