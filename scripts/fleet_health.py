#!/usr/bin/env python3
"""Fleet health: the stalls and blockers a lead otherwise finds by hand.

The lead's lane-health cron runs this every 15 minutes and acts on each line.
It exits 1 when anything needs the lead, else 0. Checks:

- disk: free space below the cargo build guard's floor stops every lane, so
  it fails there and warns within DISK_WARN_MARGIN of it.
- Astra jobs (Codex companion state): running with a dead pid; running with
  no log or session write for STALL_MINUTES; marked finished while its
  session file is still being written (the companion reports done early);
  finished since the last run, so it needs review, flagged louder when its
  final output names a blocker (disk guard, no Metal device, sandbox or
  permission refusal).
- seats: fewer running Astra jobs than SEATS.
- slots: uncommitted edits untouched for ORPHAN_MINUTES that no running job
  or process names, which is finished work nobody picked up.
- GPU lock: held by a live pid for over 30 minutes (a hung proof).

Seen-job state lives in ~/.cache/manifold/fleet-health.json.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MAIN_CHECKOUT = Path("/Users/peterkiemann/MANIFOLD - Rust")
WORKTREES = MAIN_CHECKOUT / ".claude/worktrees"
CODEX_STATE = Path.home() / ".claude/plugins/data/codex-openai-codex/state"
CODEX_SESSIONS = Path.home() / ".codex/sessions"
CACHE = Path.home() / ".cache/manifold"
SEEN_FILE = CACHE / "fleet-health.json"

GIB = 2 ** 30
DISK_FLOOR = 50 * GIB  # storage_budget.MAINTENANCE_GOAL_BYTES; the build guard refuses below it
DISK_WARN_MARGIN = 15 * GIB
SEATS = 6
STALL_MINUTES = 20
ORPHAN_MINUTES = 30
LATE_WRITE_SECONDS = 60
SEED_AGE_HOURS = 1  # with no seen-state yet, older finished jobs are history, not news
HANDOFF_FILES = {"WORKTREE_HANDOFF.md"}  # left uncommitted on purpose

BLOCKERS = (
    ("no Metal device", re.compile(r"No Metal device", re.I)),
    ("disk guard", re.compile(r"GiB free|build guard|storage admission|No space left", re.I)),
    ("sandbox refusal", re.compile(r"Operation not permitted|sandbox (?:denied|refus)", re.I)),
    ("permission", re.compile(r"Permission denied", re.I)),
    ("says blocked", re.compile(r"\b(?:blocked|cannot continue|could not proceed|stopped because)\b", re.I)),
)

# These are admission blockers that can occur while a Codex job remains
# marked running.  Keep the expressions tied to an observed refusal/result;
# words in the original request ("if approval review refuses...") are not
# evidence that the job is currently blocked.
RUNNING_BLOCKERS = (
    ("disk admission", re.compile(
        r"(?:storage|disk)\s+admission\s+(?:reported|returned|refused|rejected|blocked)"
        r"|storage\s+admission[^\n]{0,100}(?:below|free against)", re.I)),
    ("approval-review refusal", re.compile(
        r"(?:automatic\s+)?approval\s+review(?:er)?\s+(?:blocked|rejected|refused)"
        r"|approval\s+review\s+was\s+(?:blocked|rejected|refused)", re.I)),
)
RUNNING_RESOLUTION = re.compile(
    r"(?:blocker|disk admission|approval review) (?:is )?(?:resolved|cleared)|"
    r"unblocked|approval (?:was )?approved|storage admission (?:allowed|passed)|"
    r"disk now has [^\n]* free", re.I)


def pid_alive(pid):
    if not pid:
        return False
    try:
        os.kill(int(pid), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except (OSError, ValueError):
        return False
    return True


def parse_time(stamp):
    return datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()


def final_output(log_text):
    """The job's final report: everything after the last 'Final output' marker."""
    marker = log_text.rfind("Final output")
    return log_text[marker:] if marker >= 0 else log_text[-4000:]


def blockers_in(text):
    return [name for name, pattern in BLOCKERS if pattern.search(text)]


def running_blockers(text):
    """Return live-job admission blockers that have not been resolved later."""
    matches = []
    for name, pattern in RUNNING_BLOCKERS:
        matches.extend((match.start(), match.end(), name) for match in pattern.finditer(text))
    if not matches:
        return []
    last = max(matches, key=lambda item: item[0])
    # An explicit resolution closes the observed blocker. An unrelated green
    # command is insufficient: the refused admission may still be pending.
    if RUNNING_RESOLUTION.search(text, last[1]):
        return []
    return sorted({name for _, _, name in matches})


def _session_evidence(path):
    """Extract assistant/tool events while excluding the user's request."""
    if not path:
        return ""
    chunks = []
    try:
        lines = path.read_text(errors="replace").splitlines()
    except OSError:
        return ""
    for line in lines:
        try:
            record = json.loads(line)
        except ValueError:
            continue
        if not isinstance(record, dict):
            continue
        if record.get("type") == "response_item":
            payload = record.get("payload", {})
            if not isinstance(payload, dict):
                continue
            if payload.get("role") == "assistant":
                for item in payload.get("content", []):
                    if isinstance(item, dict) and isinstance(item.get("text"), str):
                        chunks.append(item["text"])
            elif payload.get("type") == "function_call_output":
                output = payload.get("output", "")
                if isinstance(output, str):
                    chunks.append(output)
                elif output:
                    chunks.append(json.dumps(output, sort_keys=True))
        elif record.get("type") == "event_msg":
            payload = record.get("payload", {})
            if not isinstance(payload, dict):
                continue
            item = payload.get("item", {})
            if not isinstance(item, dict):
                continue
            if item.get("type") == "UserMessage":
                continue
            # Command/tool results carry the useful evidence in output fields;
            # command text can repeat the user's prompt and is deliberately
            # excluded to avoid prompt false positives.
            for key in ("stdout", "stderr", "aggregated_output", "formatted_output",
                        "output", "result", "error"):
                value = item.get(key)
                if isinstance(value, str) and value:
                    chunks.append(value)
    return "\n".join(chunks)


def running_job_evidence(log, session):
    """Prefer current session events; fall back to the companion log."""
    session_text = _session_evidence(session)
    if session_text:
        return session_text
    if log:
        try:
            return log.read_text(errors="replace")
        except OSError:
            pass
    return ""


def mtime(path):
    try:
        return path.stat().st_mtime
    except OSError:
        return None


def session_file(thread_id):
    if not thread_id or not CODEX_SESSIONS.is_dir():
        return None
    matches = sorted(CODEX_SESSIONS.glob(f"*/*/*/rollout-*-{thread_id}.jsonl"))
    return matches[-1] if matches else None


def load_jobs():
    jobs = []
    if not CODEX_STATE.is_dir():
        return jobs
    for state in CODEX_STATE.glob("*/state.json"):
        try:
            data = json.loads(state.read_text())
        except (OSError, ValueError):
            continue
        for job in data.get("jobs", []):
            if Path(job.get("workspaceRoot", "")) == MAIN_CHECKOUT:
                jobs.append(job)
    return jobs


def load_seen():
    try:
        return json.loads(SEEN_FILE.read_text())
    except (OSError, ValueError):
        return {}


def save_seen(seen):
    CACHE.mkdir(parents=True, exist_ok=True)
    tmp = SEEN_FILE.with_suffix(f".tmp.{os.getpid()}")
    tmp.write_text(json.dumps(seen))
    os.replace(tmp, SEEN_FILE)


def check_disk(problems):
    free = shutil.disk_usage(MAIN_CHECKOUT).free
    if free < DISK_FLOOR:
        problems.append(("FAIL", f"disk: {free / GIB:.0f} GiB free, under the {DISK_FLOOR // GIB} GiB build guard; every cargo run is refused. Clean merged slots' targets."))
    elif free < DISK_FLOOR + DISK_WARN_MARGIN:
        problems.append(("WARN", f"disk: {free / GIB:.0f} GiB free, near the {DISK_FLOOR // GIB} GiB build guard."))


def check_jobs(jobs, seen, now, problems):
    first_run = not seen
    running = 0
    for job in jobs:
        jid, status = job.get("id", "?"), job.get("status")
        log = Path(job["logFile"]) if job.get("logFile") else None
        session = session_file(job.get("threadId"))
        touched = max(filter(None, (mtime(log) if log else None, mtime(session) if session else None)), default=None)
        if status in ("running", "queued"):
            running += 1
            if status == "running" and job.get("pid") and not pid_alive(job["pid"]):
                problems.append(("FAIL", f"{jid}: marked running but pid {job['pid']} is dead. Read its log, then relaunch with continuation context."))
            elif touched and now - touched > STALL_MINUTES * 60:
                problems.append(("FAIL", f"{jid}: running, no log or session write for {(now - touched) / 60:.0f} min. Stalled; cancel and relaunch."))
            evidence = running_job_evidence(log, session)
            found = running_blockers(evidence)
            if found:
                problems.append(("FAIL", f"{jid}: running but blocked ({', '.join(found)}); resolve the blocker or run the refused command. Read: codex-companion.mjs result {jid}"))
            continue
        if status not in ("completed", "failed", "cancelled"):
            continue
        updated = parse_time(job["updatedAt"])
        if session and (late := mtime(session)) and late - updated > LATE_WRITE_SECONDS and now - late < STALL_MINUTES * 60:
            problems.append(("WARN", f"{jid}: marked {status} but its session was still writing {(late - updated) / 60:.0f} min later. Do not trust its tree until the session goes quiet."))
        if seen.get(jid) == status:
            continue
        seen[jid] = status
        if first_run and now - updated > SEED_AGE_HOURS * 3600:
            continue
        text = ""
        if log:
            try:
                text = final_output(log.read_text(errors="replace"))
            except OSError:
                pass
        found = blockers_in(text)
        if status == "failed" or found:
            problems.append(("FAIL", f"{jid}: {status}, needs unblocking ({', '.join(found) or 'job failed'}). Read: codex-companion.mjs result {jid}"))
        elif status == "completed":
            problems.append(("ACT", f"{jid}: completed, needs review and commit. Read: codex-companion.mjs result {jid}"))
    if running < SEATS:
        problems.append(("ACT", f"seats: {running}/{SEATS} Astra jobs running; fill free seats from the ledger."))
    return running


def process_commands():
    try:
        out = subprocess.run(["ps", "-Ao", "command"], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return ""
    return out


def dirty_paths(slot):
    try:
        out = subprocess.run(["git", "-C", str(slot), "status", "--porcelain", "-uall"],
                             capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return []
    paths = []
    for line in out.splitlines():
        path = line[3:].split(" -> ")[-1].strip('"')
        if path and Path(path).name not in HANDOFF_FILES:
            paths.append(slot / path)
    return paths


def slot_named(slot_name, text):
    return re.search(rf"\b{re.escape(slot_name)}\b(?!\d)", text) is not None


def check_slots(jobs, now, problems):
    if not WORKTREES.is_dir():
        return
    live = " ".join(j.get("request", {}).get("prompt", "") for j in jobs if j.get("status") in ("running", "queued"))
    procs = process_commands()
    for slot in sorted(WORKTREES.glob("slot-*")):
        paths = dirty_paths(slot)
        if not paths:
            continue
        newest = max((m for m in map(mtime, paths) if m), default=None)
        if newest is None or now - newest < ORPHAN_MINUTES * 60:
            continue
        if slot_named(slot.name, live) or str(slot) in procs:
            continue
        problems.append(("ACT", f"{slot.name}: {len(paths)} uncommitted paths untouched for {(now - newest) / 60:.0f} min, no job or process on it. Review and commit, or hand it to a lane."))


def check_gpu_lock(problems):
    try:
        text = (CACHE / "gpu.holder").read_text()
    except OSError:
        return
    info = dict(line.partition("=")[::2] for line in text.splitlines() if "=" in line)
    pid = info.get("pid")
    if pid and not pid_alive(pid):
        return  # the flock is already free; the record is overwritten by the next holder
    since = float(info.get("since", "0") or 0)
    if pid and since and time.time() - since > 30 * 60:
        problems.append(("WARN", f"GPU lock: held {(time.time() - since) / 60:.0f} min by pid {pid} ({info.get('label', '?')[:80]}). Check it is not hung."))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--no-mark", action="store_true", help="report finished jobs without marking them seen")
    args = parser.parse_args(argv)
    now = time.time()
    problems = []
    seen = load_seen()
    jobs = load_jobs()
    check_disk(problems)
    running = check_jobs(jobs, seen, now, problems)
    check_slots(jobs, now, problems)
    check_gpu_lock(problems)
    if not args.no_mark:
        save_seen(seen)
    for level, line in problems:
        print(f"{level:4} {line}")
    print(f"fleet-health: {running} Astra running, {len(problems)} item(s)")
    return 1 if any(level in ("FAIL", "ACT") for level, _ in problems) else 0


if __name__ == "__main__":
    sys.exit(main())
