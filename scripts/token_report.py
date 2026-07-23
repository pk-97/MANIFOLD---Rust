#!/usr/bin/env python3
"""Measure Claude Code token spend from local transcripts.

Source of truth for docs/TOKEN_ECONOMICS.md. Every number in that doc came from
this script; re-run it rather than quoting the doc, which ages the moment the
roster or the workflow changes.

  python3 .claude/scripts/token_report.py             # 30-day totals by model
  python3 .claude/scripts/token_report.py --days 2    # recent window
  python3 .claude/scripts/token_report.py --daily     # per-day trend
  python3 .claude/scripts/token_report.py --sessions  # concentration + context growth
  python3 .claude/scripts/token_report.py --tools     # tool mix by seat type
  python3 .claude/scripts/token_report.py --turns     # user turns vs model calls
  python3 .claude/scripts/token_report.py --cost      # price the window at list rates

Reads ~/.claude/projects/**/*.jsonl (the per-message `usage` block Claude Code
writes locally). Deduped by message id, so resumed/copied sessions don't double.
"""
import argparse
import collections
import datetime
import glob
import json
import os

ROOT = os.path.expanduser("~/.claude/projects")

# published per-MTok rates, 2026-07-23 (input, output, cache_read, cache_write)
RATES = {
    "claude-opus-4-8":           (5.00, 25.00, 0.50, 6.25),
    "claude-fable-5":            (5.00, 25.00, 0.50, 6.25),  # no published rate; Opus as floor
    "claude-sonnet-5":           (2.00, 10.00, 0.20, 2.50),
    "claude-haiku-4-5-20251001": (1.00,  5.00, 0.10, 1.25),
    "k3":                        (0.60,  2.50, 0.80, 0.00),
    "kimi-for-coding":           (0.60,  2.50, 0.80, 0.00),
    "kimi-for-coding-highspeed": (0.60,  2.50, 0.80, 0.00),
    # non-Claude tiers, for re-pricing the same volume
    "deepseek-v4-flash":         (0.14,  0.28, 0.028, 0.00),
    "glm-5.2":                   (1.40,  4.40, 0.26,  0.00),
}


def records(days):
    """Yield (timestamp, model, usage, seat_kind, path) for the window, deduped."""
    cutoff = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=days)
    seen = set()
    for path in glob.glob(os.path.join(ROOT, "**", "*.jsonl"), recursive=True):
        kind = "subagent" if "subagents" in path else "main"
        try:
            fh = open(path, errors="replace")
        except OSError:
            continue
        with fh:
            for line in fh:
                if '"usage"' not in line:
                    continue
                try:
                    o = json.loads(line)
                except ValueError:
                    continue
                msg = o.get("message") or {}
                usage = msg.get("usage")
                if not isinstance(usage, dict):
                    continue
                ts = o.get("timestamp")
                if not ts:
                    continue
                try:
                    t = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
                except ValueError:
                    continue
                if t < cutoff:
                    continue
                key = msg.get("id") or o.get("uuid")
                if key:
                    if key in seen:
                        continue
                    seen.add(key)
                yield t, msg.get("model") or "unknown", usage, kind, path, msg


def tally(usage, counter):
    counter["input"] += usage.get("input_tokens", 0) or 0
    counter["output"] += usage.get("output_tokens", 0) or 0
    counter["cache_read"] += usage.get("cache_read_input_tokens", 0) or 0
    counter["cache_write"] += usage.get("cache_creation_input_tokens", 0) or 0
    counter["msgs"] += 1


def by_model(days, show_cost):
    agg = collections.defaultdict(collections.Counter)
    for _, model, usage, _, _, _ in records(days):
        tally(usage, agg[model])
    M = 1_000_000
    hdr = f"{'model':<28}{'msgs':>9}{'in MTok':>10}{'out MTok':>10}{'cacheR MTok':>13}"
    if show_cost:
        hdr += f"{'USD list':>11}"
    print(f"window: last {days} days")
    print(hdr)
    total = collections.Counter()
    cost_total = 0.0
    for model, c in sorted(agg.items(), key=lambda kv: -kv[1]["cache_read"]):
        row = (f"{model:<28}{c['msgs']:>9,}{c['input']/M:>10.2f}"
               f"{c['output']/M:>10.2f}{c['cache_read']/M:>13.2f}")
        if show_cost:
            r = RATES.get(model)
            k = ((c["input"]*r[0] + c["output"]*r[1] + c["cache_read"]*r[2]
                  + c["cache_write"]*r[3]) / M) if r else 0.0
            cost_total += k
            row += f"{k:>11,.0f}" if r else f"{'—':>11}"
        print(row)
        total.update(c)
    row = (f"{'TOTAL':<28}{total['msgs']:>9,}{total['input']/M:>10.2f}"
           f"{total['output']/M:>10.2f}{total['cache_read']/M:>13.2f}")
    if show_cost:
        row += f"{cost_total:>11,.0f}"
    print(row)
    if total["msgs"]:
        print(f"\nper message: {total['cache_read']/total['msgs']:,.0f} cache-read in, "
              f"{total['output']/total['msgs']:,.0f} out")
    if show_cost:
        print(f"run rate: ${cost_total/days:,.0f}/day  ->  ${cost_total/days*30:,.0f}/month")


def daily(days):
    day = collections.defaultdict(collections.Counter)
    for t, _, usage, _, _, _ in records(days):
        tally(usage, day[t.date().isoformat()])
    print(f"{'date':<12}{'calls':>9}{'cacheR GTok':>14}{'out MTok':>11}")
    for k in sorted(day):
        c = day[k]
        print(f"{k:<12}{c['msgs']:>9,}{c['cache_read']/1e9:>14.2f}{c['output']/1e6:>11.2f}")


def sessions(days):
    sess = collections.defaultdict(collections.Counter)
    order = collections.defaultdict(list)
    for _, _, usage, kind, path, _ in records(days):
        sess[path]["cache_read"] += usage.get("cache_read_input_tokens", 0) or 0
        sess[path]["calls"] += 1
        sess[path][kind] = 1
        order[path].append(usage.get("cache_read_input_tokens", 0) or 0)
    total = sum(c["cache_read"] for c in sess.values())
    print(f"sessions: {len(sess):,}   total cache-read {total/1e9:.1f} GTok\n")

    cum = n = 0
    for _, c in sorted(sess.items(), key=lambda kv: -kv[1]["cache_read"]):
        cum += c["cache_read"]
        n += 1
        if cum >= total * 0.5:
            print(f"top {n} sessions ({n/len(sess)*100:.1f}%) burn 50% of all tokens\n")
            break

    for kind in ("main", "subagent"):
        sel = [c for c in sess.values() if c.get(kind)]
        cr = sum(c["cache_read"] for c in sel)
        calls = sum(c["calls"] for c in sel)
        if calls:
            print(f"{kind:<10}{cr/total*100:>6.1f}% of tokens   "
                  f"avg context {cr/calls/1000:>6,.0f}K")

    print("\ncontext growth inside a session:")
    buckets = collections.defaultdict(lambda: [0, 0])
    for calls in order.values():
        if len(calls) < 20:
            continue
        for i, v in enumerate(calls):
            b = (i // 50) * 50
            buckets[b][0] += v
            buckets[b][1] += 1
    print(f"  {'call #':<14}{'avg context':>14}{'samples':>10}")
    for b in sorted(buckets):
        s, cnt = buckets[b]
        if cnt < 15:
            continue
        print(f"  {f'{b}-{b+49}':<14}{s/cnt/1000:>13,.0f}K{cnt:>10,}")

    short = [sum(c) for c in order.values() if 0 < len(c) < 100]
    long_ = [sum(c) for c in order.values() if len(c) >= 400]
    if short and long_:
        a, b = sum(short)/len(short), sum(long_)/len(long_)
        print(f"\n  <100 calls: {a/1e6:>7,.0f} MTok avg   "
              f">=400 calls: {b/1e6:>7,.0f} MTok avg   -> {b/a:.0f}x")


def tools(days):
    tool = collections.defaultdict(collections.Counter)
    ctx = collections.defaultdict(lambda: [0, 0])
    for _, _, usage, kind, _, msg in records(days):
        content = msg.get("content")
        names = ([b.get("name") for b in content
                  if isinstance(b, dict) and b.get("type") == "tool_use"]
                 if isinstance(content, list) else [])
        if not names:
            tool[kind]["(no tool — prose)"] += 1
        for n in names:
            tool[kind][n] += 1
        ctx[kind][0] += usage.get("cache_read_input_tokens", 0) or 0
        ctx[kind][1] += 1
    for kind in ("main", "subagent"):
        t = tool[kind]
        tot = sum(t.values())
        if not tot:
            continue
        avg = ctx[kind][0] / max(ctx[kind][1], 1) / 1000
        print(f"=== {kind}  ({tot:,} calls, avg context {avg:,.0f}K) ===")
        for n, v in t.most_common(10):
            print(f"   {str(n):<28}{v:>9,}{v/tot*100:>7.1f}%")
        print()


def turns(days):
    """User turns vs model calls — the currency z.ai and similar plans meter in."""
    cutoff = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=days)
    seen = set()
    u_turns = m_calls = 0
    for path in glob.glob(os.path.join(ROOT, "**", "*.jsonl"), recursive=True):
        try:
            fh = open(path, errors="replace")
        except OSError:
            continue
        with fh:
            for line in fh:
                if '"timestamp"' not in line:
                    continue
                try:
                    o = json.loads(line)
                except ValueError:
                    continue
                ts = o.get("timestamp")
                if not ts:
                    continue
                try:
                    t = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
                except ValueError:
                    continue
                if t < cutoff:
                    continue
                key = o.get("uuid")
                if key:
                    if key in seen:
                        continue
                    seen.add(key)
                msg = o.get("message") or {}
                role = msg.get("role") or o.get("type")
                if role == "assistant" and msg.get("usage"):
                    m_calls += 1
                elif role == "user":
                    c = msg.get("content")
                    if isinstance(c, str):
                        u_turns += 1
                    elif isinstance(c, list) and not any(
                            isinstance(b, dict) and b.get("type") == "tool_result" for b in c):
                        u_turns += 1
    print(f"user turns   {u_turns:>9,}   ({u_turns/days*7:,.0f}/week)")
    print(f"model calls  {m_calls:>9,}")
    if u_turns:
        print(f"calls/turn   {m_calls/u_turns:>9.1f}   "
              f"(z.ai sizes a 'prompt' at 15-20)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--days", type=int, default=30)
    ap.add_argument("--daily", action="store_true")
    ap.add_argument("--sessions", action="store_true")
    ap.add_argument("--tools", action="store_true")
    ap.add_argument("--turns", action="store_true")
    ap.add_argument("--cost", action="store_true")
    a = ap.parse_args()
    if a.daily:
        daily(a.days if a.days != 30 else 14)
    elif a.sessions:
        sessions(a.days if a.days != 30 else 14)
    elif a.tools:
        tools(a.days if a.days != 30 else 14)
    elif a.turns:
        turns(a.days if a.days != 30 else 14)
    else:
        by_model(a.days, a.cost)


if __name__ == "__main__":
    main()
