#!/usr/bin/env python3
"""
Standalone test for slice_fires.py's grade-join addressability fix
(2026-07-05 review): sessions self-graded with stray vocabulary (TP/FP,
y/n) because RUNBOOK's format description used those words as examples, and
55/74 grade records carried seq:null because nothing told the session the
fire's own seq. Exercises _normalize_grade_field, load_grades, and
join_grades directly against synthetic data — never touches the real
eval/live_grades*.jsonl, a real transcript, or PROJECT_DIR.

Run: python3 .claude/daemon/test_slice_fires.py
"""
import importlib.util
import json
import sys
import tempfile
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("slice_fires", DAEMON_DIR / "slice_fires.py")
sf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sf)

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def with_temp_eval(records_by_file, fn):
    """Write {filename: [records]} under a temp eval/ dir and monkeypatch
    sf.DAEMON_DIR for the duration (module-attribute patching — load_grades
    reads DAEMON_DIR / "eval" at call time, so this is enough). Never
    touches the real .claude/daemon/eval/."""
    orig = sf.DAEMON_DIR
    with tempfile.TemporaryDirectory() as td:
        tdp = Path(td)
        (tdp / "eval").mkdir()
        for name, records in records_by_file.items():
            with open(tdp / "eval" / name, "w", encoding="utf-8") as f:
                for rec in records:
                    f.write(json.dumps(rec) + "\n")
        sf.DAEMON_DIR = tdp
        try:
            fn()
        finally:
            sf.DAEMON_DIR = orig


# ---- _normalize_grade_field ----


def test_normalize_true_synonyms():
    for v in ("TP", "tp", "true", "TRUE", "y", "Y"):
        out, changed = sf._normalize_grade_field(v)
        check(f"{v!r} normalizes to True", out is True, out)
        check(f"{v!r} reports changed", changed is True, changed)


def test_normalize_false_synonyms():
    for v in ("FP", "fp", "false", "FALSE", "n", "N"):
        out, changed = sf._normalize_grade_field(v)
        check(f"{v!r} normalizes to False", out is False, out)
        check(f"{v!r} reports changed", changed is True, changed)


def test_normalize_passes_through_unrecognized_and_canonical_values():
    for v in ("miss", "unclear", "hit", "n/a", None, True, False):
        out, changed = sf._normalize_grade_field(v)
        check(f"{v!r} passes through unchanged", out is v or out == v, out)
        check(f"{v!r} reports unchanged", changed is False, changed)


# ---- load_grades ----


def test_load_grades_normalizes_and_splits_by_seq_presence():
    records = {
        "live_grades.session.jsonl": [
            {"session_id": "s1", "seq": 3, "move_id": "anchor/verify-claim", "correct": "TP", "effective": "y"},
            {"session_id": "s1", "seq": None, "move_id": "anchor/scope-drift", "correct": "FP", "effective": "n"},
            {"session_id": "s2", "seq": None, "move_id": "anchor/scope-drift", "correct": "miss", "effective": "unclear"},
        ]
    }

    def run():
        by_seq, by_move_null, normalized, total = sf.load_grades()
        check("total counts every record", total == 3, total)
        check("4 fields normalized total (TP/y on rec1, FP/n on rec2, miss/unclear untouched)", normalized == 4, normalized)
        check("seq record lands in by_seq", by_seq.get(("s1", 3), [{}])[0].get("correct") is True, by_seq)
        check("seq record's effective also normalized", by_seq[("s1", 3)][0].get("effective") is True, by_seq)
        check(
            "null-seq record lands in by_move_null keyed by (session, move)",
            ("s1", "anchor/scope-drift") in by_move_null,
            by_move_null,
        )
        check(
            "miss/unclear pass through unnormalized",
            by_move_null[("s2", "anchor/scope-drift")][0]["correct"] == "miss",
            by_move_null,
        )

    with_temp_eval(records, run)


def test_load_grades_counts_total_across_multiple_files():
    records = {
        "live_grades.jsonl": [{"session_id": "s1", "seq": 1, "move_id": "anchor/verify-claim", "correct": True}],
        "live_grades.session.jsonl": [{"session_id": "s1", "seq": 2, "move_id": "anchor/thrash", "correct": "FP"}],
    }

    def run():
        by_seq, _by_move_null, normalized, total = sf.load_grades()
        check("total spans both files", total == 2, total)
        check("only the FP record needed normalizing", normalized == 1, normalized)
        check("both seq records present", set(by_seq) == {("s1", 1), ("s1", 2)}, by_seq)

    with_temp_eval(records, run)


# ---- join_grades ----


def test_join_grades_exact_seq_match_wins():
    by_seq = {("s1", 3): [{"correct": True}]}
    by_move_null = {("s1", "anchor/verify-claim"): [{"correct": "miss"}]}
    grecs, ambiguous = sf.join_grades("s1", 3, "anchor/verify-claim", by_seq, by_move_null, {("s1", "anchor/verify-claim"): 2})
    check("exact seq match returned", grecs == [{"correct": True}], grecs)
    check("exact match is never ambiguous", ambiguous is False, ambiguous)


def test_join_grades_fallback_unambiguous_when_move_fired_once():
    by_move_null = {("s1", "anchor/scope-drift"): [{"correct": False}]}
    grecs, ambiguous = sf.join_grades("s1", None, "anchor/scope-drift", {}, by_move_null, {("s1", "anchor/scope-drift"): 1})
    check("fallback attached", grecs == [{"correct": False}], grecs)
    check("single firing is unambiguous", ambiguous is False, ambiguous)


def test_join_grades_fallback_ambiguous_when_move_fired_more_than_once():
    by_move_null = {("s1", "anchor/scope-drift"): [{"correct": False}]}
    grecs, ambiguous = sf.join_grades("s1", None, "anchor/scope-drift", {}, by_move_null, {("s1", "anchor/scope-drift"): 3})
    check("fallback still attached (never silently dropped)", grecs == [{"correct": False}], grecs)
    check("multiple firings flagged ambiguous", ambiguous is True, ambiguous)


def test_join_grades_no_grades_available():
    grecs, ambiguous = sf.join_grades("s1", 9, "anchor/verify-claim", {}, {}, {})
    check("no grades -> empty list", grecs == [], grecs)
    check("no grades -> not ambiguous", ambiguous is False, ambiguous)


def test_join_grades_no_move_and_no_seq_is_empty_not_a_crash():
    grecs, ambiguous = sf.join_grades("s1", None, None, {}, {("s1", None): [{"correct": True}]}, {})
    check("no move id -> empty list, no exception", grecs == [], grecs)
    check("no move id -> not ambiguous", ambiguous is False, ambiguous)


def main():
    tests = [
        test_normalize_true_synonyms,
        test_normalize_false_synonyms,
        test_normalize_passes_through_unrecognized_and_canonical_values,
        test_load_grades_normalizes_and_splits_by_seq_presence,
        test_load_grades_counts_total_across_multiple_files,
        test_join_grades_exact_seq_match_wins,
        test_join_grades_fallback_unambiguous_when_move_fired_once,
        test_join_grades_fallback_ambiguous_when_move_fired_more_than_once,
        test_join_grades_no_grades_available,
        test_join_grades_no_move_and_no_seq_is_empty_not_a_crash,
    ]
    for t in tests:
        t()
    for name in PASS:
        print(f"PASS: {name}")
    for name, detail in FAIL:
        print(f"FAIL: {name} ({detail!r})")
    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
