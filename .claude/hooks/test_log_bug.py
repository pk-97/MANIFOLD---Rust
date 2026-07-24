#!/usr/bin/env python3
"""Tests for log_bug.py's collision-safe id allocator (bump_counter/mint_id) —
the fix for two concurrent worktrees minting the same BUG number.

Run: python3 .claude/hooks/test_log_bug.py
"""
import importlib.util
import tempfile
import threading
from pathlib import Path

MOD_PATH = Path(__file__).resolve().parent / "log_bug.py"
spec = importlib.util.spec_from_file_location("log_bug_under_test", MOD_PATH)
lb = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lb)

PASS = []
FAIL = []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    if not cond:
        print(f"FAIL: {name} {detail}")


def fresh():
    d = Path(tempfile.mkdtemp())
    return d / "seq", d / "lock"


# 1. Empty counter + floor N -> N+1, and persists.
seq, lock = fresh()
check("empty counter respects floor", lb.bump_counter(seq, lock, 327) == 328)
check("counter persisted", seq.read_text().strip() == "328")

# 2. Monotonic: repeated calls at a LOWER floor keep climbing (never reuse).
seq, lock = fresh()
seq.write_text("400\n")
got = [lb.bump_counter(seq, lock, 10) for _ in range(3)]
check("counter ignores lower floor, stays monotonic", got == [401, 402, 403], got)

# 3. Self-heal: a floor ABOVE the counter (a merge imported higher ids) wins.
seq, lock = fresh()
seq.write_text("5\n")
check("floor above counter self-heals", lb.bump_counter(seq, lock, 500) == 501)
check("counter caught up", seq.read_text().strip() == "501")

# 4. Corrupt counter file falls back to floor, doesn't crash.
seq, lock = fresh()
seq.write_text("not-a-number\n")
check("corrupt counter tolerated", lb.bump_counter(seq, lock, 42) == 43)

# 5. Concurrency: many threads racing ONE counter leave with DISTINCT numbers
#    (each open()+flock is an independent lock holder; the counter must serialize
#    them). Distinctness holds regardless of timing when the lock works.
seq, lock = fresh()
results = []
rlock = threading.Lock()


def worker():
    n = lb.bump_counter(seq, lock, 0)
    with rlock:
        results.append(n)


threads = [threading.Thread(target=worker) for _ in range(50)]
for t in threads:
    t.start()
for t in threads:
    t.join()
check("50 concurrent callers get distinct ids", len(set(results)) == 50, sorted(results))
check("concurrent ids are contiguous 1..50", sorted(results) == list(range(1, 51)))

# 6. scan_max reads the largest visible id (index + entries), floor for mint.
head = ["# Bug backlog", "| BUG-100 | x |", "| BUG-322 | y |"]

class _E:
    def __init__(self, i): self.id = i

# archive_ids may hit the real archive file; only assert the head/entries floor.
sm = lb.scan_max(head, [_E("BUG-150"), _E("BUG-322")])
check("scan_max picks the max visible", sm >= 322, sm)

print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
raise SystemExit(1 if FAIL else 0)
