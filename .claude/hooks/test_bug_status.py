#!/usr/bin/env python3
"""
Standalone tests for bug_status.py's parse/rebuild/write — the BUG-139 class:
## Fixed archive-pointer lines must be a first-class bucket, never swallowed
into a preceding entry's body (false --check noise) and never dropped by
rebuild()/write() (content loss).

Run: python3 .claude/hooks/test_bug_status.py
"""
import importlib.util
from pathlib import Path

MOD_PATH = Path(__file__).resolve().parent / "bug_status.py"
spec = importlib.util.spec_from_file_location("bug_status_under_test", MOD_PATH)
bs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bs)

PASS = []
FAIL = []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name)
    if not cond:
        print(f"FAIL: {name} {detail}")


HEAD = """# Bug backlog

| BUG-001 | open bug one-liner |

## Open

### BUG-001 (open-slug) — something broken — MED
**Status:** OPEN

**Symptom:** x.
"""

PTRS = (
    "- BUG-003 (archived-slug) — FIXED — full history in docs/archive/BUG_BACKLOG_CLOSED.md\n"
    "- BUG-004 (archived-two) — FIXED (2026-07-01) — full history in docs/archive/BUG_BACKLOG_CLOSED.md\n"
)

TAIL = "\n## Checked and safe\n\n- nothing\n"

# Shape A — the 2026-07-13 symptom: a full resolved entry leads ## Fixed, the
# pointer list follows it. Old parse() swallowed the pointers into BUG-002's body.
SHAPE_A = HEAD + """
## Fixed

### BUG-002 (fixed-slug) — was broken — FIXED 2026-07-02
**Status:** FIXED (2026-07-02)

**Symptom:** y.

""" + PTRS + TAIL

# Shape B — the original symptom: pointers only, no leading full entry.
# Old parse() bucketed them as strays; rebuild() dropped them.
SHAPE_B = HEAD + "\n## Fixed\n\n" + PTRS + TAIL


def run_shape(label, text):
    head, entries, tail, strays, pointers = bs.parse(text)
    check(f"{label}: two pointer lines parsed", len(pointers) == 2, f"got {pointers!r}")
    check(f"{label}: pointer ids", bs.pointer_ids(pointers) == {"BUG-003", "BUG-004"})
    check(f"{label}: no pointer text in any entry body",
          not any("full history in" in l for e in entries for l in e.lines[1:]))
    check(f"{label}: no pointers among strays",
          not any(bs.POINTER_RE.match(s.strip()) for s in strays))
    out = bs.write(text)
    for ptr in PTRS.strip().split("\n"):
        check(f"{label}: pointer survives write ({ptr[:14]}…)", ptr in out)
    # write() must be a fixpoint: a second pass changes nothing
    check(f"{label}: write is idempotent", bs.write(out) == out)


run_shape("A(entry+pointers)", SHAPE_A)
run_shape("B(pointers only)", SHAPE_B)

# entry classification unaffected: BUG-002 still parses as a resolved full entry
_, entries_a, _, _, _ = bs.parse(SHAPE_A)
check("A: BUG-002 still a full FIXED entry",
      any(e.id == "BUG-002" and e.status == "FIXED" for e in entries_a))

print(f"{len(PASS)} passed, {len(FAIL)} failed")
raise SystemExit(1 if FAIL else 0)
