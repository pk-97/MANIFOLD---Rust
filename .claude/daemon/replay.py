#!/usr/bin/env python3
"""Offline replay harness for the daemon observer classifier.

Replays historical session transcripts through the same windowing the live
daemon will use (common.WindowState), calls the Haiku classifier over each
window, and reports whether the go-live gates in DESIGN.md §4 hold:

  - incident recall: expected-family flag fires before the user's correction
    message in >=60% of labeled incidents
  - clean-session noise: <1 false flag per clean session on average
  - precision: hand-checked separately from `report --dump-fires`

Two subcommands:
  run    <run.py run --out results.json>   builds windows, calls the model,
         saves raw per-window verdicts. This is the only subcommand that
         spends money — always `--dry-run` first to see the call count.
  report <run.py report --in results.json> re-derives gate numbers from a
         saved run, applying cooldown suppression. Free — no API calls — so
         cooldown/gate-logic tuning doesn't require re-spending.

Incident-labeled sessions are truncated once every incident marker for that
session has been matched (first live occurrence, per DESIGN.md's compaction-
replay note) — nothing after the last marker affects the recall gate, and
some labeled sessions run to thousands of tool events. Clean sessions always
replay in full, since the noise gate needs the whole session's flag count.
"""

import argparse
import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402

DAEMON_DIR = os.path.dirname(os.path.abspath(__file__))
MOVES_PATH = os.path.join(DAEMON_DIR, "moves.md")
RUBRIC_PATH = os.path.join(DAEMON_DIR, "rubric.md")
LABELS_PATH = os.path.join(DAEMON_DIR, "eval", "labels.jsonl")
SESSIONS_DIR = os.path.expanduser("~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust")

EST_COST_PER_CALL = 0.007  # observed on claude-haiku-4-5-20251001 with NEUTRAL_CWD, see commit note
CIRCUIT_BREAKER = 25  # consecutive error verdicts before a run aborts
MAX_VALID_ERROR_RATE = 0.05  # a report above this is not a measurement


def load_labels():
    with open(LABELS_PATH) as f:
        return [json.loads(l) for l in f if l.strip()]


def group_by_session(labels):
    by_session = {}
    for lbl in labels:
        by_session.setdefault(lbl["session"], []).append(lbl)
    return by_session


def build_session_windows(path, incident_labels):
    """Returns (windows, marker_hits). marker_hits maps label index (within
    incident_labels) -> (event_count, ts) of the first live occurrence of
    that label's marker text. Truncates the replay once every marker in
    incident_labels has been found — see module docstring."""
    remaining = {i: lbl["marker"].strip() for i, lbl in enumerate(incident_labels)}
    marker_hits = {}
    truncate = bool(incident_labels)

    state = common.WindowState()
    windows = []

    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                d = json.loads(line)
            except json.JSONDecodeError:
                continue
            etype = d.get("type")
            if etype not in ("user", "assistant"):
                continue
            content = d.get("message", {}).get("content")
            ts = common.parse_ts(d.get("timestamp"))

            if etype == "assistant":
                # Assistant content is always a block list in practice.
                if not isinstance(content, list) or not content:
                    continue
                closed = state.feed_assistant_content(content, ts, model=d.get("message", {}).get("model"))
                if closed:
                    windows.append(closed)
            else:
                if content is None or (isinstance(content, list) and not content):
                    continue
                closed, human_texts = state.feed_user_content(content, ts)
                if closed:
                    windows.append(closed)
                for text in human_texts:
                    for idx in list(remaining.keys()):
                        marker = remaining[idx]
                        if text.startswith(marker) or marker in text:
                            marker_hits[idx] = (state.total_tool_event_count, ts)
                            del remaining[idx]

            if truncate and not remaining and marker_hits:
                break

    return windows, marker_hits


def classify_with_retry(system_prompt, window_text, retries=3, base_delay=5):
    """replay.py wants a real answer per window (an "error" verdict silently
    depresses the recall gate), unlike the live daemon which is allowed to
    fail open on any single miss. Retry transient failures (rate limits,
    timeouts) with backoff; give up and return the error after `retries`."""
    verdict = common.call_classifier(system_prompt, window_text)
    attempt = 0
    while "error" in verdict and attempt < retries:
        time.sleep(base_delay * (2**attempt))
        attempt += 1
        verdict = common.call_classifier(system_prompt, window_text)
    return verdict


def cmd_run(args):
    labels = load_labels()
    by_session = group_by_session(labels)
    if args.sessions:
        wanted = set(args.sessions.split(","))
        by_session = {k: v for k, v in by_session.items() if k in wanted}

    moves = common.parse_moves(common.read(MOVES_PATH))
    system_prompt = common.build_system_prompt(common.read(RUBRIC_PATH), moves)

    sessions_out = {}
    total_windows = 0
    for session, lbls in sorted(by_session.items()):
        path = os.path.join(SESSIONS_DIR, session)
        if not os.path.exists(path):
            print(f"WARN: missing transcript {session}", file=sys.stderr)
            continue
        kind = lbls[0]["kind"]
        incident_labels = [l for l in lbls if l["kind"] == "incident"]
        windows, marker_hits = build_session_windows(path, incident_labels)

        # Cost cuts for tuning rounds (the final gate run should use
        # --tail 0 --clean-cap 0 to measure everything):
        # - incidents: recall only needs a fire BEFORE the marker, and drift
        #   develops in the run-up — keep the last N windows before each
        #   marker. Conservative: can only under-count recall.
        # - cleans: stride-sample to a cap; noise-per-window still measures.
        if kind == "incident" and args.tail and marker_hits:
            keep = set()
            for ec, _ts in marker_hits.values():
                pre = [i for i, w in enumerate(windows) if w["end_event_count"] <= ec]
                keep.update(pre[-args.tail:])
            windows = [w for i, w in enumerate(windows) if i in keep]
        elif kind == "clean" and args.clean_cap and len(windows) > args.clean_cap:
            n = args.clean_cap
            idxs = {round(i * (len(windows) - 1) / (n - 1)) for i in range(n)}
            windows = [w for i, w in enumerate(windows) if i in idxs]

        total_windows += len(windows)
        sessions_out[session] = {
            "kind": kind,
            "windows": windows,
            "marker_hits": {str(i): list(v) for i, v in marker_hits.items()},
        }
        print(f"{session}: {len(windows)} windows, {len(marker_hits)}/{len(incident_labels)} markers matched")

    est_cost = total_windows * EST_COST_PER_CALL
    print(f"\nTotal windows to classify: {total_windows}  (~${est_cost:.2f} at ~${EST_COST_PER_CALL}/call)")

    if args.dry_run:
        return

    if args.limit_windows:
        for s in sessions_out.values():
            s["windows"] = s["windows"][: args.limit_windows]

    # --resume: reuse non-error verdicts from a previous (partial) run so an
    # aborted run's paid-for calls are never re-spent.
    reused = 0
    if args.resume:
        with open(args.resume) as f:
            prior = json.load(f)["sessions"]
        prior_verdicts = {
            (s, w["end_event_count"]): w["verdict"]
            for s, data in prior.items()
            for w in data["windows"]
            if "verdict" in w and "error" not in (w["verdict"] or {"error": True})
        }
        for session, data in sessions_out.items():
            for w in data["windows"]:
                v = prior_verdicts.get((session, w["end_event_count"]))
                if v is not None:
                    w["verdict"] = v
                    reused += 1
        print(f"Resumed {reused} verdicts from {args.resume}")

    # Content-addressed cache: identical (rubric, window) pairs are never
    # re-bought, across runs and across sessions. Rubric edits change the key,
    # so stale answers can't leak through.
    cache = common.VerdictCache(os.path.join(DAEMON_DIR, "eval", "verdict_cache.jsonl"))
    cache_hits = 0
    for session, data in sessions_out.items():
        for w in data["windows"]:
            if "verdict" not in w:
                v = cache.get(system_prompt, w["text"])
                if v is not None:
                    w["verdict"] = v
                    cache_hits += 1
    if cache_hits:
        print(f"Verdict cache: {cache_hits} hits")

    tasks = [(session, w) for session, data in sessions_out.items() for w in data["windows"] if "verdict" not in w]
    print(f"Dispatching {len(tasks)} classifier calls with {args.workers} workers...")
    done = 0
    consecutive_errors = 0
    aborted = False
    with ThreadPoolExecutor(max_workers=args.workers) as ex:
        futs = {ex.submit(classify_with_retry, system_prompt, w["text"]): w for _, w in tasks}
        for fut in as_completed(futs):
            w = futs[fut]
            try:
                verdict = fut.result()
            except Exception as e:  # noqa: BLE001 - any subprocess/threading failure becomes an error verdict
                verdict = {"error": str(e)}
            w["verdict"] = verdict
            cache.put(system_prompt, w["text"], verdict)
            done += 1
            # Circuit breaker: a sustained error run means quota/auth/rate wall,
            # not per-window flakiness. Stop burning calls; partial results are
            # saved and a later --resume run pays only for what's missing.
            consecutive_errors = consecutive_errors + 1 if "error" in verdict else 0
            if consecutive_errors >= CIRCUIT_BREAKER:
                aborted = True
                ex.shutdown(cancel_futures=True)
                break
            if done % 50 == 0 or done == len(tasks):
                print(f"  {done}/{len(tasks)} done")

    with open(args.out, "w") as f:
        json.dump({"sessions": sessions_out}, f, indent=2)
    errors = sum(
        1 for data in sessions_out.values() for w in data["windows"] if "error" in (w.get("verdict") or {})
    )
    print(f"Wrote raw results to {args.out}  ({errors} error verdicts)")
    if aborted:
        print(
            f"ABORTED: {CIRCUIT_BREAKER} consecutive classifier errors — quota/rate wall. "
            f"Fix the cause, then re-run with --resume {args.out}",
            file=sys.stderr,
        )
        sys.exit(1)


def compute_gates(raw, labels_by_session, moves):
    incident_results = []
    clean_flag_counts = []
    all_effective_fires = []
    move_cooldowns = {mid: m["cooldown"] for mid, m in moves.items()}

    for session, data in raw["sessions"].items():
        windows = sorted(data["windows"], key=lambda w: w["end_event_count"])
        fires = []
        for w in windows:
            flag = common.validate_move_id((w.get("verdict") or {}).get("flag"), moves)
            if flag:
                fires.append((w["end_event_count"], flag, w))
        effective = common.apply_cooldowns(fires, move_cooldowns)
        all_effective_fires.extend((session, ec, mid, win) for ec, mid, win in effective)

        session_labels = labels_by_session.get(session, [])
        kind = session_labels[0]["kind"] if session_labels else "unknown"

        if kind == "clean":
            clean_flag_counts.append((session, len(effective)))
            continue

        marker_hits = data.get("marker_hits", {})
        incident_labels = [l for l in session_labels if l["kind"] == "incident"]
        for idx, lbl in enumerate(incident_labels):
            hit_info = marker_hits.get(str(idx))
            expect = lbl.get("expect_family")
            if hit_info is None:
                incident_results.append(
                    {"session": session, "marker": lbl["marker"], "expect_family": expect, "status": "marker_not_found"}
                )
                continue
            marker_ec, _marker_ts = hit_info
            if expect is None:
                incident_results.append(
                    {"session": session, "marker": lbl["marker"], "expect_family": None, "status": "rubric_gap"}
                )
                continue
            target_families = {expect} | set(lbl.get("accept_also") or [])
            hit = any(mid in target_families and ec <= marker_ec for ec, mid, _ in effective)
            incident_results.append(
                {"session": session, "marker": lbl["marker"], "expect_family": expect, "status": "hit" if hit else "miss"}
            )

    scored = [r for r in incident_results if r["status"] in ("hit", "miss")]
    recall = (sum(1 for r in scored if r["status"] == "hit") / len(scored)) if scored else None
    avg_false = (sum(c for _, c in clean_flag_counts) / len(clean_flag_counts)) if clean_flag_counts else None

    return {
        "recall": recall,
        "recall_hits": sum(1 for r in scored if r["status"] == "hit"),
        "recall_total": len(scored),
        "incident_results": incident_results,
        "avg_false_flags_per_clean_session": avg_false,
        "clean_flag_counts": clean_flag_counts,
        "all_effective_fires": all_effective_fires,
    }


def cmd_report(args):
    with open(args.infile) as f:
        raw = json.load(f)
    by_session = group_by_session(load_labels())
    moves = common.parse_moves(common.read(MOVES_PATH))
    gates = compute_gates(raw, by_session, moves)

    # Validity first: gate numbers from a run full of classifier errors are
    # not measurements. An error verdict has no flag, so unchecked it reads
    # as "clear" and silently zeroes recall — the exact verify-claim failure
    # this system exists to catch.
    all_windows = [w for data in raw["sessions"].values() for w in data["windows"]]
    unclassified = sum(1 for w in all_windows if "verdict" not in w)
    errored = sum(1 for w in all_windows if "error" in (w.get("verdict") or {}))
    bad = unclassified + errored
    rate = bad / len(all_windows) if all_windows else 0.0
    print(f"=== Run validity ===")
    print(f"  {len(all_windows)} windows: {len(all_windows) - bad} classified, {errored} errors, {unclassified} never run")
    if rate > MAX_VALID_ERROR_RATE:
        print(f"  *** {rate:.0%} unusable — GATES INVALID. Re-run (use --resume) before reading any number below. ***")

    print("\n=== Incident recall (gate: >=60%) ===")
    if gates["recall"] is None:
        print("  n/a — no scored incidents")
    else:
        print(f"  {gates['recall_hits']}/{gates['recall_total']} = {gates['recall']:.0%}")
    for r in gates["incident_results"]:
        print(f"  [{r['status']:>16}] {r['session'][:20]}  expect={r['expect_family']!s:<28} marker={r['marker'][:55]!r}")

    print("\n=== Clean-session noise (gate: <1 avg false flag) ===")
    if gates["avg_false_flags_per_clean_session"] is None:
        print("  n/a — no clean sessions in this run")
    else:
        print(f"  avg = {gates['avg_false_flags_per_clean_session']:.2f}")
    for session, count in gates["clean_flag_counts"]:
        print(f"  {session[:20]}: {count} flags")

    print(f"\n=== All effective fires (n={len(gates['all_effective_fires'])}) — spot-check a sample for precision ===")
    for session, ec, mid, win in gates["all_effective_fires"]:
        verdict = win.get("verdict") or {}
        evidence = (verdict.get("evidence") or "")[:80]
        print(f"  {session[:20]} @event{ec:<5} [{mid:<28}] conf={verdict.get('confidence')}  evidence={evidence!r}")

    if args.dump_fires:
        with open(args.dump_fires, "w") as f:
            json.dump(
                [
                    {"session": s, "end_event_count": ec, "flag": mid, "window": win}
                    for s, ec, mid, win in gates["all_effective_fires"]
                ],
                f,
                indent=2,
            )
        print(f"\nWrote fire details to {args.dump_fires}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    run_p = sub.add_parser("run", help="build windows + call the classifier (spends money)")
    run_p.add_argument("--out", required=True)
    run_p.add_argument("--sessions", help="comma-separated session filenames to limit to")
    run_p.add_argument("--workers", type=int, default=8)
    run_p.add_argument("--limit-windows", type=int, help="cap windows per session (smoke tests)")
    run_p.add_argument("--dry-run", action="store_true", help="print window counts + cost estimate, no API calls")
    run_p.add_argument("--resume", help="previous run's --out file; reuse its non-error verdicts instead of re-paying")
    run_p.add_argument("--tail", type=int, default=25, help="incident sessions: last N windows before each marker (0 = all; use 0 for the final gate run)")
    run_p.add_argument("--clean-cap", type=int, default=40, help="clean sessions: stride-sample to N windows (0 = all; use 0 for the final gate run)")
    run_p.set_defaults(func=cmd_run)

    report_p = sub.add_parser("report", help="recompute gates from a saved run (free)")
    report_p.add_argument("--in", dest="infile", required=True)
    report_p.add_argument("--dump-fires", help="write all effective fires (with evidence) to this JSON path")
    report_p.set_defaults(func=cmd_report)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
