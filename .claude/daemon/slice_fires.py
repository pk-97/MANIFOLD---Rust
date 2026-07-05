#!/usr/bin/env python3
"""Cut per-fire transcript slices for the sleep pass (RUNBOOK step 1).

Joins telemetry.jsonl `injected` records to their transcripts, locates each
<daemon move="..."> block, and writes one compact, readable slice per fire
(context window before + ~40 events after) so the grading model reads
pre-cut slices instead of hunting through whole transcripts.

Usage (from .claude/daemon/):
    python3 slice_fires.py            # writes eval/slices/ + index.md
    python3 slice_fires.py --before 25 --after 40

Outputs are gitignored (transcript content never enters the repo).
"""

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import tool_label, tool_result_status, tool_target  # noqa: E402

DAEMON_DIR = Path(__file__).resolve().parent
TELEMETRY = DAEMON_DIR / "telemetry.jsonl"
SLICES_DIR = DAEMON_DIR / "eval" / "slices"
PROJECT_DIR = Path.home() / ".claude" / "projects" / "-Users-peterkiemann-MANIFOLD---Rust"

UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
DAEMON_BLOCK_RE = re.compile(r'<daemon(?:-advice)? move="([^"]+)"[^>]*>(.*?)</daemon(?:-advice)?>', re.DOTALL)

TEXT_CLIP = 700
RESULT_CLIP = 160


def clip(s, n):
    s = " ".join((s or "").split())
    return s if len(s) <= n else s[: n - 1] + "…"


def load_jsonl(path):
    out = []
    if not path.exists():
        return out
    for line in path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out


def load_fires():
    """Real injected fires from telemetry, oldest first, per-session order index."""
    fires, skipped = [], 0
    for rec in load_jsonl(TELEMETRY):
        if rec.get("event") != "injected":
            continue
        if not UUID_RE.match(rec.get("session_id", "")):
            skipped += 1  # test-fixture residue (sess-* ids) — not live fires
            continue
        fires.append(rec)
    fires.sort(key=lambda r: r.get("ts", 0))
    order = {}
    for f in fires:
        key = (f["session_id"], f.get("agent_id"))
        order[key] = order.get(key, 0) + 1
        f["_nth"] = order[key]  # nth injection in this transcript (main or per-agent), by time
    return fires, skipped


# 2026-07-05 addressability fix: sessions self-graded with stray vocabulary
# (TP/FP, y/n) because RUNBOOK's format description used those words as
# examples and they leaked into the actual corpus. Normalize only what's
# named here — anything else (already-bool, "miss", "unclear", or an
# unrecognized string like "hit"/"n/a") passes through untouched rather than
# guessing meaning nobody defined.
_TRUE_SYNONYMS = {"tp", "true", "y"}
_FALSE_SYNONYMS = {"fp", "false", "n"}


def _normalize_grade_field(value):
    """Fold TP/true/y -> True and FP/false/n -> False (case-insensitive).
    Returns (value, changed)."""
    if isinstance(value, str):
        low = value.strip().lower()
        if low in _TRUE_SYNONYMS:
            return True, True
        if low in _FALSE_SYNONYMS:
            return False, True
    return value, False


def load_grades():
    """Split every grade record (from all live_grades*.jsonl files) into the
    two join targets the addressability fix needs: `by_seq` for records that
    carry the fire's own seq (the exact join), `by_move_null` for records
    that don't — keyed on (session_id, move_id), the fallback join, safe only
    when that move fired exactly once in the session (see `join_grades`).
    correct/effective are normalized in place via `_normalize_grade_field`.

    Returns (by_seq, by_move_null, normalized_count, total_count) — both
    counts are printed by main(), never silently dropped."""
    by_seq, by_move_null = {}, {}
    normalized = total = 0
    for path in sorted((DAEMON_DIR / "eval").glob("live_grades*.jsonl")):
        for rec in load_jsonl(path):
            total += 1
            for field in ("correct", "effective"):
                if field in rec:
                    new, changed = _normalize_grade_field(rec[field])
                    rec[field] = new
                    if changed:
                        normalized += 1
            sid, seq, move = rec.get("session_id"), rec.get("seq"), rec.get("move_id")
            if seq is not None:
                by_seq.setdefault((sid, seq), []).append(rec)
            else:
                by_move_null.setdefault((sid, move), []).append(rec)
    return by_seq, by_move_null, normalized, total


def join_grades(sid, seq, move, by_seq, by_move_null, move_counts):
    """Attach grade records to one fire: exact (session_id, seq) match first;
    else the (session_id, move_id) fallback for seq:null records, flagged
    AMBIGUOUS when that move fired more than once in the session
    (`move_counts`, built from the real telemetry fires, not the grades) —
    nothing else distinguishes which fallback grade belongs to which firing.
    Returns (grade_records, ambiguous_bool)."""
    exact = by_seq.get((sid, seq), [])
    if exact:
        return exact, False
    if not move:
        return [], False
    fallback = by_move_null.get((sid, move), [])
    if not fallback:
        return [], False
    return fallback, move_counts.get((sid, move), 0) > 1


def load_scored():
    scored = {}
    for rec in load_jsonl(TELEMETRY):
        if rec.get("event") == "scored":
            scored[(rec.get("session_id"), rec.get("seq"))] = rec
    return scored


FIRE_LOG_RE = re.compile(r"^(\d{2}):(\d{2}):(\d{2}) ([a-z]+/[a-z0-9-]+) fired", re.MULTILINE)


def load_log_fires(sid):
    """Observer-log fires for a session: (local seconds-of-day, move_id).

    Worker injections reach the subagent's live context but are never
    persisted to its transcript jsonl, so the observer log is the only
    surviving record of WHICH move fired for them.
    """
    log = DAEMON_DIR / "verdicts" / f"{sid}.log"
    if not log.exists():
        return []
    return [
        (int(h) * 3600 + int(m) * 60 + int(s), move)
        for h, m, s, move in FIRE_LOG_RE.findall(log.read_text(errors="replace"))
    ]


def move_from_mailbox(sid, aid, seq):
    """Exact recovery from a surviving verdict mailbox (last fire per target only)."""
    name = f"{sid}.{aid}.json" if aid else f"{sid}.json"
    path = DAEMON_DIR / "verdicts" / name
    if not path.exists():
        return None, None
    try:
        flag = (json.loads(path.read_text()) or {}).get("flag") or {}
    except (json.JSONDecodeError, OSError):
        return None, None
    if flag.get("seq") == seq:
        return flag.get("move_id"), flag.get("evidence")
    return None, None


def move_from_log(log_fires, fire_ts, tolerance=180):
    local = datetime.fromtimestamp(fire_ts)  # observer logs use local time
    sod = local.hour * 3600 + local.minute * 60 + local.second
    best = min(log_fires, key=lambda p: abs(p[0] - sod), default=None)
    if best and abs(best[0] - sod) <= tolerance:
        return best[1]
    return None


def parse_line_ts(raw):
    try:
        return datetime.fromisoformat(raw.replace("Z", "+00:00")).timestamp()
    except (ValueError, AttributeError, TypeError):
        return None


def render_events(transcript_path):
    """Flatten a transcript into (label_lines, is_daemon_block, move_id, ts) events."""
    events = []
    for raw in transcript_path.read_text(errors="replace").splitlines():
        try:
            rec = json.loads(raw)
        except json.JSONDecodeError:
            continue
        rtype = rec.get("type")
        ts = parse_line_ts(rec.get("timestamp"))
        if rtype == "assistant":
            for block in (rec.get("message") or {}).get("content") or []:
                if block.get("type") == "text" and block.get("text", "").strip():
                    events.append((f"[assistant] {clip(block['text'], TEXT_CLIP)}", None, ts))
                elif block.get("type") == "tool_use":
                    input_ = block.get("input") or {}
                    label = tool_label(block.get("name", "?"), input_)
                    target = tool_target(input_)
                    events.append((f"[tool] {label} {clip(target, 140)}".rstrip(), None, ts))
        elif rtype == "user":
            content = (rec.get("message") or {}).get("content")
            if isinstance(content, str):
                if content.strip():
                    events.append((f"[user] {clip(content, TEXT_CLIP)}", None, ts))
                continue
            for block in content or []:
                if block.get("type") == "text":
                    text = block.get("text", "")
                    m = DAEMON_BLOCK_RE.search(text)
                    if m:
                        events.append((f"[DAEMON INJECTION move={m.group(1)}]\n{m.group(2).strip()}", m.group(1), ts))
                    elif text.strip():
                        events.append((f"[user] {clip(text, TEXT_CLIP)}", None, ts))
                elif block.get("type") == "tool_result":
                    status = tool_result_status(block)
                    if events and events[-1][0].startswith("[tool] ") and " -> " not in events[-1][0]:
                        events[-1] = (f"{events[-1][0]} -> {status}", events[-1][1], events[-1][2])
                    else:
                        events.append((f"[result {status}]", None, ts))
        elif rtype == "attachment":
            text = json.dumps(rec.get("attachment") or {})
            m = DAEMON_BLOCK_RE.search(text.encode().decode("unicode_escape", errors="replace"))
            if m:
                events.append((f"[DAEMON INJECTION move={m.group(1)}]\n{m.group(2).strip()}", m.group(1), ts))
    return events


def slice_for_fire(events, fire, before, after):
    """Locate the fire's daemon block (nth in session, else nearest-ts) and cut."""
    block_idxs = [i for i, (_, move, _) in enumerate(events) if move]
    idx, how = None, ""
    if len(block_idxs) >= fire["_nth"]:
        idx, how = block_idxs[fire["_nth"] - 1], "matched-by-order"
    else:
        timed = [(i, ts) for i, (_, _, ts) in enumerate(events) if ts]
        if timed:
            idx = min(timed, key=lambda p: abs(p[1] - fire["ts"]))[0]
            how = "UNMATCHED-nearest-timestamp"
    if idx is None:
        return None, "no-anchor", None
    move = events[idx][1]
    lo, hi = max(0, idx - before), min(len(events), idx + after + 1)
    return [e[0] for e in events[lo:hi]], how, move


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--before", type=int, default=25)
    ap.add_argument("--after", type=int, default=40)
    args = ap.parse_args()

    fires, skipped_test = load_fires()
    by_seq, by_move_null, normalized_count, total_grades = load_grades()
    scored = load_scored()
    SLICES_DIR.mkdir(parents=True, exist_ok=True)
    for old in SLICES_DIR.glob("*.md"):
        old.unlink()

    # Pass 1: resolve each fire's move_id (transcript block / mailbox /
    # observer log, same fallback chain as before) and count real fires per
    # (session_id, move_id) — the ambiguity criterion `join_grades` needs for
    # the seq:null fallback join (2026-07-05 addressability fix).
    cache, resolved, move_counts = {}, [], {}
    for fire in fires:
        sid, seq, aid = fire["session_id"], fire.get("seq"), fire.get("agent_id")
        if aid:
            transcript = PROJECT_DIR / sid / "subagents" / f"agent-{aid}.jsonl"
        else:
            transcript = PROJECT_DIR / f"{sid}.jsonl"
        if not transcript.exists():
            resolved.append(None)
            continue
        ckey = (sid, aid)
        if ckey not in cache:
            cache[ckey] = render_events(transcript)
        lines, how, move = slice_for_fire(cache[ckey], fire, args.before, args.after)
        if lines is None:
            resolved.append((None, how, None, None))
            continue
        evidence = None
        if move is None:
            move, evidence = move_from_mailbox(sid, aid, seq)
            if move:
                how += "+move-from-mailbox"
        if move is None:
            move = move_from_log(load_log_fires(sid), fire["ts"])
            if move:
                how += "+move-from-observer-log"
        resolved.append((lines, how, move, evidence))
        if move:
            move_counts[(sid, move)] = move_counts.get((sid, move), 0) + 1

    index, ambiguous_count = [], 0
    for n, (fire, res) in enumerate(zip(fires, resolved), 1):
        sid, seq, aid = fire["session_id"], fire.get("seq"), fire.get("agent_id")
        when = datetime.fromtimestamp(fire["ts"], tz=timezone.utc).strftime("%m-%d %H:%M")
        if res is None:
            index.append((n, sid, seq, "?", when, "TRANSCRIPT MISSING", None))
            continue
        lines, how, move, evidence = res
        if lines is None:
            index.append((n, sid, seq, "?", when, how, None))
            continue

        srec = scored.get((sid, seq))
        grecs, ambiguous = join_grades(sid, seq, move, by_seq, by_move_null, move_counts)
        if ambiguous:
            ambiguous_count += len(grecs)
        who = f"worker-{aid[:6]}" if aid else "main"
        name = f"{n:02d}_{sid[:8]}_{who}_seq{seq}_{(move or 'unknown').replace('/', '-')}.md"
        header = [
            f"# Fire {n}: {move or 'unknown'} ({who})",
            f"- session `{sid}` seq {seq} · {when} UTC · valve {fire.get('valve', '?')} · anchor {how}",
            f"- mechanical score: {srec.get('outcome') if srec else 'none'}",
        ]
        if evidence:
            header.append(f"- classifier evidence: {clip(evidence, 300)}")
        for g in grecs:
            tag = "AMBIGUOUS fallback (no seq, move fired >1x this session) — " if ambiguous else ""
            header.append(
                f"- prior grade ({g.get('grader') or 'pass'}): {tag}correct={g.get('correct')} "
                f"effective={g.get('effective')} — {clip(g.get('notes', ''), 200)}"
            )
        (SLICES_DIR / name).write_text("\n".join(header) + "\n\n```\n" + "\n\n".join(lines) + "\n```\n")
        index.append((n, sid, seq, move, when, how, name))

    idx_lines = [
        "# Fire slices — sleep-pass grading input",
        f"{len(fires)} live fires ({skipped_test} test-fixture records skipped). "
        f"Grades already attached where they exist.",
        f"Grade vocab normalized: {normalized_count} field value(s) across {total_grades} "
        f"grade records (TP/true/y -> true, FP/false/n -> false; everything else "
        f"passes through unchanged).",
        f"Ambiguous seq:null fallback grades: {ambiguous_count} (move fired more than "
        f"once in the session — joining by move_id alone couldn't tell them apart; "
        f"see the AMBIGUOUS tags above).",
        "",
    ]
    for n, sid, seq, move, when, how, name in index:
        target = f"[{name}]({name})" if name else f"**{how}**"
        idx_lines.append(f"- {n:02d} · {when} · `{sid[:8]}` seq {seq} · {move} · {target}")
    (SLICES_DIR / "index.md").write_text("\n".join(idx_lines) + "\n")
    print(f"{len([i for i in index if i[6]])}/{len(fires)} fires sliced -> {SLICES_DIR}")
    print(
        f"grade normalization: {normalized_count}/{total_grades} field values folded; "
        f"{ambiguous_count} ambiguous fallback grades — never silently shrunk, see index.md"
    )
    unmatched = [i for i in index if not i[6] or "UNMATCHED" in i[5]]
    if unmatched:
        print(f"{len(unmatched)} need attention (missing transcript / unmatched anchor) — see index.md")


if __name__ == "__main__":
    main()
