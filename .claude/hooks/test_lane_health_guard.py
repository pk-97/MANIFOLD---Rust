#!/usr/bin/env python3
"""Standalone test runner for lane-health-guard.py.

Invokes decide()/armed_job_present() directly with synthetic stores in a tmp
dir — never spawns a real hook subprocess against a live session.

Run: python3 .claude/hooks/test_lane_health_guard.py
"""
import importlib.util
import json
import tempfile
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
HOOK_PATH = HOOKS_DIR / "lane-health-guard.py"
spec = importlib.util.spec_from_file_location("lane_health_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


TMP = tempfile.TemporaryDirectory()
ROOT = Path(TMP.name)
STORE_DIR = ROOT / ".claude"
STORE_DIR.mkdir()
STORE = STORE_DIR / "scheduled_tasks.json"


def write_store(tasks) -> None:
    STORE.write_text(json.dumps({"tasks": tasks}), encoding="utf-8")


def marker_task(**over):
    task = {
        "id": "deadbeef",
        "cron": "7-59/10 * * * *",
        "prompt": f"{hook.MARKER}: per lane — is a build/test process running",
        "createdAt": 1780000000000,
        "recurring": True,
    }
    task.update(over)
    return task


# --- decide(): run_in_background gate --------------------------------------

write_store([marker_task()])
check(
    "sync spawn (run_in_background=false) passes with no store check",
    hook.decide({"run_in_background": False}, ["/nonexistent-dir"]) == "",
)
check(
    "background spawn (explicit true) with armed job passes",
    hook.decide({"run_in_background": True}, [str(ROOT)]) == "",
)
check(
    "background spawn (absent flag = harness default) with armed job passes",
    hook.decide({}, [str(ROOT)]) == "",
)

# --- decide(): missing store denies ----------------------------------------

d = hook.decide({}, ["/nonexistent-a", "/nonexistent-b"])
check("missing store denies", d.startswith("Background Agent spawn denied"))
check("missing-store deny names corrected form", hook.MARKER in d and "durable=true" in d)
check("missing-store deny lists checked paths", "/nonexistent-a" in d and "/nonexistent-b" in d)

# --- decide(): store exists, no qualifying task ----------------------------

write_store([])
d = hook.decide({}, [str(ROOT)])
check("empty tasks denies", "no recurring task" in d and hook.MARKER in d)

write_store([marker_task(recurring=False)])
d = hook.decide({}, [str(ROOT)])
check("one-shot marker job does not count (recurring required)", "no recurring task" in d)

write_store([marker_task(prompt="health check for lanes")])
d = hook.decide({}, [str(ROOT)])
check("prompt without marker string does not count", "no recurring task" in d)

write_store([{"id": hook.MARKER, "cron": "* * * * *", "prompt": "x", "createdAt": 1, "recurring": True}])
d = hook.decide({}, [str(ROOT)])
check("marker outside prompt does not count", "no recurring task" in d)

write_store([{"prompt": "garbage"}, None, 42, marker_task()])
check("malformed sibling tasks do not hide an armed job", hook.decide({}, [str(ROOT)]) == "")

# --- decide(): unparseable store fails closed with a named fix -------------

STORE.write_text("{not json", encoding="utf-8")
d = hook.decide({}, [str(ROOT)])
check("unparseable store denies", d.startswith("Background Agent spawn denied"))
check("unparseable deny names the file and fix", str(STORE) in d and "Fix or delete" in d)

STORE.write_text('{"tasks": {"not": "a list"}}', encoding="utf-8")
d = hook.decide({}, [str(ROOT)])
check("non-list tasks denies as unparseable-shape", "no 'tasks' list" in d)

# --- find_store(): candidate order, first existing wins ---------------------

OTHER = ROOT / "other"
(OTHER / ".claude").mkdir(parents=True)
(OTHER / ".claude" / "scheduled_tasks.json").write_text('{"tasks": []}', encoding="utf-8")
write_store([marker_task()])
check(
    "first existing candidate wins",
    hook.find_store(["/nonexistent", str(ROOT), str(OTHER)]) == str(STORE),
)
check("none existing returns None", hook.find_store(["/nonexistent-x", ""]) is None)
check(
    "armed job in second candidate passes when first has none",
    hook.decide({}, ["/nonexistent", str(ROOT)]) == "",
)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURE(S)")
    raise SystemExit(1)
print("ALL PASS")
