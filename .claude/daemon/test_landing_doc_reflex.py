#!/usr/bin/env python3
"""
Standalone test for DESIGN.md §2h.2's mechanical/landing-doc-reflex:
common.py's detect_landing_on_main (ported preToolUseBash.py command
recognition) and observer.py's deterministic mechanical fire path
(_check_landing_doc_reflex, _landing_docs_only_suppresses). Never spawns a
real classifier call — the mechanical fire path bypasses Haiku entirely, by
design, mirroring test_stopgap_detection.py / test_git_landing_detection.py's
pattern for the sibling mechanical moves.

Run: python3 .claude/daemon/test_landing_doc_reflex.py
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
from pathlib import Path

DAEMON_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(DAEMON_DIR))
import common  # noqa: E402

spec = importlib.util.spec_from_file_location("observer", DAEMON_DIR / "observer.py")
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
import valve  # noqa: E402

PASS, FAIL = [], []


def check(name, cond, detail=""):
    (PASS if cond else FAIL).append(name if cond else (name, detail))


def with_temp_dirs(fn):
    orig_verdicts = observer.VERDICTS_DIR
    orig_flag = observer.WORKER_NUDGES_FLAG
    orig_valve_verdicts, orig_valve_flag = valve.VERDICTS_DIR, valve.WORKER_NUDGES_FLAG
    with tempfile.TemporaryDirectory() as td:
        observer.VERDICTS_DIR = td
        observer.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        valve.VERDICTS_DIR = td
        valve.WORKER_NUDGES_FLAG = os.path.join(td, "worker-nudges.enabled")
        try:
            fn(td)
        finally:
            observer.VERDICTS_DIR, observer.WORKER_NUDGES_FLAG = orig_verdicts, orig_flag
            valve.VERDICTS_DIR, valve.WORKER_NUDGES_FLAG = orig_valve_verdicts, orig_valve_flag


REPO = "/Users/peterkiemann/repo"  # a plausible, never-resolved cwd — no real filesystem I/O needed for the no-`-C` case


# ---- common.detect_landing_on_main: recognition table ----


def test_push_explicit_main_refspec_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git push origin main"}, REPO, "feature/whatever")
    check("push origin main fires regardless of current branch", "push" in hits, hits)


def test_push_explicit_refs_heads_main_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git push origin HEAD:refs/heads/main"}, REPO, "feature/x")
    check("push ...:refs/heads/main fires", "push" in hits, hits)


def test_push_no_refspec_on_main_branch_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git push"}, REPO, "main")
    check("bare push while on main fires", "push" in hits, hits)


def test_push_no_refspec_off_main_branch_does_not_fire():
    hits = common.detect_landing_on_main("Bash", {"command": "git push"}, REPO, "feat/thing")
    check("bare push while on a feature branch does not fire", hits == [], hits)


def test_push_explicit_feature_branch_does_not_fire():
    hits = common.detect_landing_on_main("Bash", {"command": "git push origin my-feature"}, REPO, "main")
    check("explicit non-main refspec does not fire even from main", hits == [], hits)


def test_merge_on_main_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git merge some-branch"}, REPO, "main")
    check("merge while on main fires", "merge" in hits, hits)


def test_merge_off_main_does_not_fire():
    hits = common.detect_landing_on_main("Bash", {"command": "git merge some-branch"}, REPO, "feat/thing")
    check("merge while on a feature branch does not fire", hits == [], hits)


def test_checkout_never_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git checkout main"}, REPO, "feat/thing")
    check("checkout is not a landing command", hits == [], hits)


def test_cherry_pick_never_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git cherry-pick abc123"}, REPO, "main")
    check("cherry-pick is not a landing command (mechanical/git-landing's territory)", hits == [], hits)


def test_switch_never_fires():
    hits = common.detect_landing_on_main("Bash", {"command": "git switch main"}, REPO, "main")
    check("switch is not a landing command", hits == [], hits)


def test_non_bash_tool_never_fires():
    hits = common.detect_landing_on_main("Edit", {"file_path": "foo.rs", "new_string": "git push origin main"}, REPO, "main")
    check("non-Bash tool never scanned", hits == [], hits)


def test_malformed_input_never_raises():
    hits = common.detect_landing_on_main("Bash", "not-a-dict", REPO, "main")
    check("malformed input returns empty, no raise", hits == [], hits)


def test_missing_cwd_never_raises_and_abstains():
    hits = common.detect_landing_on_main("Bash", {"command": "git push"}, None, "main")
    check("missing cwd abstains, no raise", hits == [], hits)


def test_dash_c_to_other_directory_abstains():
    hits = common.detect_landing_on_main(
        "Bash", {"command": f'git -C "/some/other/worktree" push'}, REPO, "main"
    )
    check("-C to a different directory abstains (can't resolve that dir's branch)", hits == [], hits)


def test_dash_c_to_same_directory_still_fires():
    hits = common.detect_landing_on_main("Bash", {"command": f'git -C "{REPO}" push'}, REPO, "main")
    check("-C back to the session's own cwd still resolves", "push" in hits, hits)


def test_compound_merge_and_push_both_detected():
    hits = common.detect_landing_on_main(
        "Bash", {"command": "git merge --no-ff feat/x && git push"}, REPO, "main"
    )
    check("merge present in compound command", "merge" in hits, hits)
    check("push present in compound command", "push" in hits, hits)


# ---- observer.py: docs-only suppression ----


def make_daemon():
    return observer.Daemon("test-session", "/dev/null")


def test_docs_only_suppresses_when_all_recent_edits_are_docs():
    d = make_daemon()
    d.state.last_edit_event = {"docs/FOO_DESIGN.md": 10, ".claude/daemon/moves.md": 12}
    check("all-docs trailing window suppresses", d._landing_docs_only_suppresses(d, event_count=15))


def test_no_recent_edits_does_not_suppress():
    d = make_daemon()
    d.state.last_edit_event = {}
    check("zero edits in window does NOT suppress (nothing established)", not d._landing_docs_only_suppresses(d, event_count=15))


def test_one_code_edit_in_window_does_not_suppress():
    d = make_daemon()
    d.state.last_edit_event = {"docs/FOO_DESIGN.md": 10, "crates/manifold-core/src/lib.rs": 11}
    check("a single code edit in the window blocks suppression", not d._landing_docs_only_suppresses(d, event_count=15))


def test_edit_outside_trailing_window_is_ignored():
    d = make_daemon()
    # A code edit from long before the window start must not count against
    # (or for) docs-only-ness; only the docs edit inside the window should.
    old_event = 15 - observer.LANDING_DOCS_WINDOW_EVENTS - 5
    d.state.last_edit_event = {"crates/manifold-core/src/lib.rs": old_event, "docs/FOO_DESIGN.md": 14}
    check("stale out-of-window edit excluded -> docs-only holds", d._landing_docs_only_suppresses(d, event_count=15))


# ---- observer.py: mechanical fire path ----


def test_check_landing_doc_reflex_fires_for_merge_on_main():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_landing_doc_reflex(
            "Bash", {"command": "git merge --no-ff feat/x"}, event_count=1, logf=logf, cwd=REPO, git_branch="main"
        )
        with open(d.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check("flag written", record.get("flag") is not None, record)
        check(
            "move_id is mechanical/landing-doc-reflex",
            record["flag"]["move_id"] == "mechanical/landing-doc-reflex",
            record,
        )
        check("window_version on record", record.get("window_version") == common.WINDOW_VERSION, record)
    with_temp_dirs(run)


def test_check_landing_doc_reflex_no_hit_writes_nothing():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        d._check_landing_doc_reflex(
            "Bash", {"command": "git checkout main"}, event_count=1, logf=logf, cwd=REPO, git_branch="feat/thing"
        )
        check("no verdict file written when clean", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_check_landing_doc_reflex_suppressed_when_docs_only():
    def run(td):
        d = make_daemon()
        d.state.last_edit_event = {"docs/BUG_BACKLOG.md": 3, "docs/landings/2026-07-07.md": 4}
        logf = io.StringIO()
        d._check_landing_doc_reflex(
            "Bash", {"command": "git push"}, event_count=5, logf=logf, cwd=REPO, git_branch="main"
        )
        check("docs-only trailing window suppresses the fire", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_check_landing_doc_reflex_fires_when_code_touched_despite_doc_edits():
    def run(td):
        d = make_daemon()
        d.state.last_edit_event = {"docs/BUG_BACKLOG.md": 3, "crates/manifold-core/src/lib.rs": 4}
        logf = io.StringIO()
        d._check_landing_doc_reflex(
            "Bash", {"command": "git push"}, event_count=5, logf=logf, cwd=REPO, git_branch="main"
        )
        check("a code edit alongside doc edits still fires", os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_check_landing_doc_reflex_respects_cooldown():
    def run(td):
        d = make_daemon()
        logf = io.StringIO()
        cmd = {"command": "git push"}
        d._check_landing_doc_reflex("Bash", cmd, event_count=1, logf=logf, cwd=REPO, git_branch="main")
        with open(d.verdict_path, encoding="utf-8") as f:
            first = json.load(f)
        with open(d.consumed_path, "w", encoding="utf-8") as f:
            f.write(str(first["flag"]["seq"]))
        d._check_landing_doc_reflex("Bash", cmd, event_count=5, logf=logf, cwd=REPO, git_branch="main")
        with open(d.verdict_path, encoding="utf-8") as f:
            second = json.load(f)
        check(
            "cooldown suppresses re-fire within 20 events",
            second["flag"]["seq"] == first["flag"]["seq"],
            (first, second),
        )
        d._check_landing_doc_reflex("Bash", cmd, event_count=25, logf=logf, cwd=REPO, git_branch="main")
        with open(d.verdict_path, encoding="utf-8") as f:
            third = json.load(f)
        check("fires again once cooldown elapses", third["flag"]["seq"] != first["flag"]["seq"], (first, third))
    with_temp_dirs(run)


def test_landing_doc_reflex_never_fires_during_catchup():
    def run(td):
        session_dir = os.path.join(td, "session")
        os.makedirs(session_dir)
        transcript = os.path.join(session_dir, "sess1.jsonl")
        with open(transcript, "w", encoding="utf-8") as f:
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": "land this on main please"},
                "timestamp": "2026-07-07T00:00:00Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "git push"}},
                ]},
                "timestamp": "2026-07-07T00:00:01Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "is_error": False, "content": ""},
                ]},
                "timestamp": "2026-07-07T00:00:02Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
        d = observer.Daemon("sess1", transcript)
        logf = io.StringIO()
        d._catchup(logf)  # classify=False internally
        check("no verdict file after catchup-only replay", not os.path.exists(d.verdict_path))
    with_temp_dirs(run)


def test_landing_doc_reflex_fires_on_live_tail():
    """Same transcript shape as the catchup test above, but drained live
    (classify=True) via _drain directly — confirms the wiring inside
    _feed_line (cwd/gitBranch plumbed through) actually fires, not just the
    unit-level _check_landing_doc_reflex call."""
    def run(td):
        session_dir = os.path.join(td, "session")
        os.makedirs(session_dir)
        transcript = os.path.join(session_dir, "sess1.jsonl")
        with open(transcript, "w", encoding="utf-8") as f:
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": "land this on main please"},
                "timestamp": "2026-07-07T00:00:00Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "git push"}},
                ]},
                "timestamp": "2026-07-07T00:00:01Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "is_error": False, "content": ""},
                ]},
                "timestamp": "2026-07-07T00:00:02Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
        d = observer.Daemon("sess1", transcript)
        logf = io.StringIO()
        d._drain(0, logf, classify=True)
        check("verdict file written on live drain", os.path.exists(d.verdict_path))
        if os.path.exists(d.verdict_path):
            with open(d.verdict_path, encoding="utf-8") as f:
                record = json.load(f)
            check(
                "fired move_id is mechanical/landing-doc-reflex",
                (record.get("flag") or {}).get("move_id") == "mechanical/landing-doc-reflex",
                record,
            )
    with_temp_dirs(run)


def test_worker_landing_doc_reflex_routes_to_agent_mailbox_when_enabled():
    def run(td):
        with open(observer.WORKER_NUDGES_FLAG, "w", encoding="utf-8") as f:
            f.write("1")
        # Real layout: <project>/<session_id>/subagents/, derived by the
        # Daemon itself (no session_dir override — the 2026-07-04 discovery
        # bug is exactly what an override would mask again).
        session_dir = os.path.join(td, "session")
        os.makedirs(os.path.join(session_dir, "sess1", "subagents"))
        agent_path = os.path.join(session_dir, "sess1", "subagents", "agent-abc123.jsonl")
        with open(agent_path, "w", encoding="utf-8") as f:
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": "land the worktree branch"},
                "timestamp": "2026-07-07T00:00:00Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "git merge feat/x"}},
                ]},
                "timestamp": "2026-07-07T00:00:01Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "is_error": False, "content": ""},
                ]},
                "timestamp": "2026-07-07T00:00:02Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
        d = observer.Daemon("sess1", os.path.join(session_dir, "sess1.jsonl"))
        logf = io.StringIO()
        d._scan_agents(logf)  # discovers + catchup (classify=False) — should NOT fire yet
        worker = d.agents.get("abc123")
        check("agent discovered", worker is not None)
        check("no fire on agent catchup", not os.path.exists(worker.verdict_path) if worker else False)

        with open(agent_path, "a", encoding="utf-8") as f:
            f.write(json.dumps({
                "type": "assistant",
                "message": {"role": "assistant", "model": "claude-sonnet-5", "content": [
                    {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "git push"}},
                ]},
                "timestamp": "2026-07-07T00:00:03Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
            f.write(json.dumps({
                "type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t2", "is_error": False, "content": ""},
                ]},
                "timestamp": "2026-07-07T00:00:04Z", "cwd": REPO, "gitBranch": "main",
            }) + "\n")
        d._scan_agents(logf)

        check("worker mailbox got the flag", os.path.exists(worker.verdict_path))
        check("session mailbox untouched by worker landing-doc-reflex fire", not os.path.exists(d.verdict_path))
        with open(worker.verdict_path, encoding="utf-8") as f:
            record = json.load(f)
        check(
            "worker flag is mechanical/landing-doc-reflex",
            record["flag"]["move_id"] == "mechanical/landing-doc-reflex",
            record,
        )
    with_temp_dirs(run)


# ---- moves.md sanity ----


def test_moves_md_has_landing_doc_reflex_entry():
    moves_text = common.read(str(DAEMON_DIR / "moves.md"))
    moves = common.parse_moves(moves_text)
    check("mechanical/landing-doc-reflex present in moves.md", "mechanical/landing-doc-reflex" in moves, sorted(moves))
    entry = moves.get("mechanical/landing-doc-reflex") or {}
    check("has a signature", bool(entry.get("signature")), entry)
    check("has a payload", bool(entry.get("payload")), entry)
    check(
        "never classifier-selectable",
        common.validate_move_id("mechanical/landing-doc-reflex", moves) is None,
    )


def main():
    tests = [
        test_push_explicit_main_refspec_fires,
        test_push_explicit_refs_heads_main_fires,
        test_push_no_refspec_on_main_branch_fires,
        test_push_no_refspec_off_main_branch_does_not_fire,
        test_push_explicit_feature_branch_does_not_fire,
        test_merge_on_main_fires,
        test_merge_off_main_does_not_fire,
        test_checkout_never_fires,
        test_cherry_pick_never_fires,
        test_switch_never_fires,
        test_non_bash_tool_never_fires,
        test_malformed_input_never_raises,
        test_missing_cwd_never_raises_and_abstains,
        test_dash_c_to_other_directory_abstains,
        test_dash_c_to_same_directory_still_fires,
        test_compound_merge_and_push_both_detected,
        test_docs_only_suppresses_when_all_recent_edits_are_docs,
        test_no_recent_edits_does_not_suppress,
        test_one_code_edit_in_window_does_not_suppress,
        test_edit_outside_trailing_window_is_ignored,
        test_check_landing_doc_reflex_fires_for_merge_on_main,
        test_check_landing_doc_reflex_no_hit_writes_nothing,
        test_check_landing_doc_reflex_suppressed_when_docs_only,
        test_check_landing_doc_reflex_fires_when_code_touched_despite_doc_edits,
        test_check_landing_doc_reflex_respects_cooldown,
        test_landing_doc_reflex_never_fires_during_catchup,
        test_landing_doc_reflex_fires_on_live_tail,
        test_worker_landing_doc_reflex_routes_to_agent_mailbox_when_enabled,
        test_moves_md_has_landing_doc_reflex_entry,
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
