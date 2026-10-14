#!/usr/bin/env python3
"""Read a MANIFOLD sync rig log and print the numbers the frame-exact transport
design needs before P3 freezes its constants (FRAME_EXACT_TRANSPORT_DESIGN.md,
commit C0).

The app writes the log on every run: ~/Library/Logs/com.latentspace.manifold/
sync-<unix ms>-<pid>-pNNNN.log (the path is printed once at startup). Parts of
one session share the name up to -pNNNN.

  scripts/dev.py sync-rig-report --latest        newest session in the log dir
  scripts/dev.py sync-rig-report FILE [FILE...]  these parts, in name order

What it reports:
  D20  content tick lateness and stalls; MIDI clock tick jitter on both stamps
       (midir's and ours) and how steady the gap between the two clocks is;
       clock ticks while the clock transport is stopped; song position pointers.
  D7   Link minus MIDI clock phase, per 5 s window and overall (mod 4 beats).
  D19  Link minus AbletonOSC song time; AbletonOSC report intervals; how late
       the tempo listener arrives after the song-time slope changes; present
       delay (presentedTime minus the tick deadline); what each display reports.
  Also the authority and transport timeline, so the numbers can be read
  against what was played, and how many records the app dropped.
"""
import argparse
import bisect
import math
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

LOG_DIR = Path.home() / "Library/Logs/com.latentspace.manifold"
QUANTUM = 4.0
PART_RE = re.compile(r"^(sync-\d+-\d+)-p\d+\.log$")


def parse(paths):
    """Records as (kind, {key: str}) in file order; free-form display lines kept raw."""
    records = []
    for path in paths:
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.rstrip("\n")
                if not line or line.startswith("#"):
                    continue
                kind, _, rest = line.partition(" ")
                if kind in ("display", "edid"):
                    records.append((kind, {"raw": rest}))
                    continue
                kv = {}
                for part in rest.split(" "):
                    k, sep, v = part.partition("=")
                    if sep:
                        kv[k] = v
                records.append((kind, kv))
    return records


def num(s):
    return float(s)  # Python reads Rust's NaN and inf spellings


def pct(xs, p):
    if not xs:
        return math.nan
    xs = sorted(xs)
    k = (len(xs) - 1) * p / 100.0
    lo, hi = math.floor(k), math.ceil(k)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def dist(xs, unit="ms", digits=3):
    if not xs:
        return "no samples"
    f = f"{{:.{digits}f}}"
    return (f"n={len(xs)} median={f.format(pct(xs, 50))} p5={f.format(pct(xs, 5))} "
            f"p95={f.format(pct(xs, 95))} p99={f.format(pct(xs, 99))} "
            f"min={f.format(min(xs))} max={f.format(max(xs))} {unit}")


def wrap(beats):
    """Phase difference folded into [-quantum/2, quantum/2)."""
    return (beats + QUANTUM / 2) % QUANTUM - QUANTUM / 2


def clock_text(ns):
    s = ns / 1e9
    return f"{int(s // 60)}:{s % 60:06.3f}"


def linfit(xs, ys):
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx == 0:
        return my, 0.0
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    return my - slope * mx, slope


class LinkTrack:
    """Link's beat at any rig time, interpolated between tick deadlines."""

    def __init__(self, ticks):
        pts = [(int(t["dl"]), num(t["lb"]), num(t["lt"])) for t in ticks
               if t["dl"] != "0" and not math.isnan(num(t["lb"]))]
        pts.sort()
        self.t = [p[0] for p in pts]
        self.b = [p[1] for p in pts]
        self.bpm = [p[2] for p in pts]

    def at(self, ns):
        i = bisect.bisect_right(self.t, ns)
        if i == 0 or i == len(self.t):
            return None
        t0, t1 = self.t[i - 1], self.t[i]
        if t1 - t0 > 100_000_000:  # a gap over 100 ms: don't interpolate across it
            return None
        b0, b1 = self.b[i - 1], self.b[i]
        if abs((b1 - b0) - (t1 - t0) / 1e9 * self.bpm[i] / 60.0) > 0.05:
            return None  # Link's timeline stepped between these two ticks
        return b0 + (b1 - b0) * (ns - t0) / (t1 - t0), self.bpm[i]


def section(title):
    print(f"\n== {title}")


def report_ticks(ticks):
    section("Content ticks (D20)")
    if not ticks:
        print("no tick records")
        return
    late = [(int(t["wake"]) - int(t["dl"])) / 1e6 for t in ticks if t["dl"] != "0"]
    print(f"wake minus deadline: {dist(late)}")
    print(f"woke more than 2 ms late: {sum(1 for x in late if x > 2.0)} of {len(late)}")
    wakes = [int(t["wake"]) for t in ticks]
    gaps = [(b - a) / 1e6 for a, b in zip(wakes, wakes[1:])]
    print(f"wake to wake: {dist(gaps)}")
    ns = [int(t["n"]) for t in ticks]
    print(f"frames per tick: 0 -> {ns.count(0)}, 1 -> {ns.count(1)}, "
          f"2+ -> {sum(1 for n in ns if n >= 2)} (max {max(ns)})")
    stalls = [(int(t["wake"]), int(t["n"])) for t in ticks if int(t["n"]) >= 3]
    for wake, n in stalls[:20]:
        print(f"  stall at {clock_text(wake)}: {n} frames in one tick")
    if len(stalls) > 20:
        print(f"  ... {len(stalls) - 20} more")


def clk_runs(clks):
    """Split clock bytes into runs between Start/Continue and Stop, with each
    0xF8's beat position. Returns (tick list, stopped tick count, SPP list)."""
    beat = None
    running = False
    stopped_ticks = 0
    spp = []
    ticks = []  # (at_ns, midir_us, beat)
    for c in clks:
        st = int(c["st"], 16)
        if st == 0xF2:
            pos = int(c["d1"]) | (int(c["d2"]) << 7)
            spp.append((int(c["t"]), pos))
            beat = pos / 4.0
        elif st == 0xFA:
            running, beat = True, 0.0
        elif st == 0xFB:
            running = True
            beat = beat if beat is not None else 0.0
        elif st == 0xFC:
            running = False
        elif st == 0xF8:
            if not running:
                stopped_ticks += 1
                continue
            ticks.append((int(c["t"]), int(c["midir_us"]), beat))
            beat += 1.0 / 24.0
    return ticks, stopped_ticks, spp


def window_residuals(xs_ns, window=96):
    """Jitter: residuals (ms) of each clock tick from a straight line fitted to
    its window of `window` ticks, so tempo drift doesn't count as jitter."""
    out = []
    for start in range(0, len(xs_ns) - window + 1, window):
        seg = xs_ns[start:start + window]
        idx = list(range(window))
        a, b = linfit(idx, seg)
        out.extend((y - (a + b * i)) / 1e6 for i, y in zip(idx, seg))
    return out


def report_clk(clks):
    section("MIDI clock (D20)")
    if not clks:
        print("no MIDI clock bytes")
        return [], []
    counts = defaultdict(int)
    for c in clks:
        counts[c["st"]] += 1
    print("bytes by status: " + ", ".join(f"{k}={v}" for k, v in sorted(counts.items())))
    ticks, stopped, spp = clk_runs(clks)
    print(f"0xF8 ticks while the clock transport was stopped: {stopped}")
    print(f"song position pointers: {len(spp)}"
          + "".join(f"\n  {clock_text(t)} -> sixteenth {p} (beat {p / 4:g})" for t, p in spp[:20]))
    # Contiguous running stretches only: a gap over 200 ms ends a stretch.
    ours, theirs = [], []
    stretch_o, stretch_m = [], []
    last = None
    for at, midir, _ in ticks:
        if last is not None and at - last > 200_000_000:
            ours.extend(window_residuals(stretch_o))
            theirs.extend(window_residuals(stretch_m))
            stretch_o, stretch_m = [], []
        stretch_o.append(at)
        stretch_m.append(midir * 1000)
        last = at
    ours.extend(window_residuals(stretch_o))
    theirs.extend(window_residuals(stretch_m))
    print(f"tick jitter, our arrival stamp: {dist(ours)}")
    if any(m for _, m, _ in ticks):
        print(f"tick jitter, midir stamp:       {dist(theirs)}")
        offs = [(at / 1e3 - midir) for at, midir, _ in ticks if midir]
        if len(offs) > 2:
            base = statistics.median(offs)
            rel = [(o - base) / 1e3 for o in offs]
            secs = [at / 1e9 for at, midir, _ in ticks if midir]
            _, slope = linfit(secs, offs)
            print(f"our stamp minus midir's, about its median: {dist(rel)}")
            print(f"  drift: {slope:.3f} us per second")
    else:
        print("midir stamps are all zero: this backend gives no timestamp")
    return ticks, spp


def report_phase(title, samples, bpm_of):
    """samples: (at_ns, source beat, link beat, link bpm)."""
    section(title)
    if not samples:
        print("no overlapping samples")
        return
    ms = [wrap(lb - sb) * 60000.0 / bpm for _, sb, lb, bpm in samples]
    print(f"overall (link minus source, folded to +-{QUANTUM / 2:g} beats): {dist(ms)}")
    windows = defaultdict(list)
    for (at, _, _, _), m in zip(samples, ms):
        windows[at // 5_000_000_000].append(m)
    print("per 5 s window: start  median  spread(p95-p5)  n")
    rows = sorted(windows.items())
    for w, xs in rows[:120]:
        print(f"  {clock_text(w * 5_000_000_000)}  {pct(xs, 50):8.2f}  {pct(xs, 95) - pct(xs, 5):7.2f}  {len(xs)}")
    if len(rows) > 120:
        print(f"  ... {len(rows) - 120} more windows")


def report_osc(songs, tempos, link):
    section("AbletonOSC reports (D19)")
    if not songs and not tempos:
        print("no AbletonOSC song time or tempo records")
        return
    st = [int(s["t"]) for s in songs]
    print(f"song time inter-arrival: {dist([(b - a) / 1e6 for a, b in zip(st, st[1:])])}")
    tt = [int(t["t"]) for t in tempos]
    print(f"tempo inter-arrival:     {dist([(b - a) / 1e6 for a, b in zip(tt, tt[1:])])}")

    # Listener lag: when did the song-time slope change, against when the tempo arrived?
    pts = [(int(s["t"]), num(s["beats"])) for s in songs]
    lags = []
    for i in range(1, len(tempos)):
        old, new = num(tempos[i - 1]["bpm"]), num(tempos[i]["bpm"])
        ta = int(tempos[i]["t"])
        if abs(new - old) < 0.5:
            continue
        if any(abs(int(t["t"]) - ta) < 4e9 and t is not tempos[i] and abs(num(t["bpm"]) - new) > 0.01
               for t in tempos[max(0, i - 50):i + 50]):
            continue  # another change nearby (a ramp): no clean break to find
        win = [(t, b) for t, b in pts if abs(t - ta) < 4e9]
        if len(win) < 8:
            continue
        best = None
        for tau in range(ta - 4_000_000_000, ta + 1_000_000_000, 1_000_000):
            pred = [((t - tau) / 6e10) * (old if t < tau else new) for t, _ in win]
            b0 = sum(b - p for (_, b), p in zip(win, pred)) / len(win)
            sse = sum((b - b0 - p) ** 2 for (_, b), p in zip(win, pred))
            if best is None or sse < best[0]:
                best = (sse, tau)
        lags.append((ta, (ta - best[1]) / 1e6, old, new))
    print(f"tempo listener lag behind the song-time slope change (both by arrival): {dist([x[1] for x in lags])}")
    for ta, lag, old, new in lags[:20]:
        print(f"  {clock_text(ta)} {old:g} -> {new:g} bpm: tempo arrived {lag:.1f} ms after the slope changed")

    samples = []
    for t, b in pts:
        hit = link.at(t)
        if hit is not None and not math.isnan(b):
            samples.append((t, b, hit[0], hit[1]))
    report_phase("Link minus AbletonOSC song time, at arrival (D19)", samples, None)


def report_present(ticks, presents, presenteds, statuses):
    section("Present delay (D19)")
    if not presents:
        print("no present records (no output window, or not macOS)")
        return
    pairs = [(int(s["t"]), num(s["host_s"])) for s in statuses if "host_s" in s]
    if not pairs:
        print("no clock pairs: cannot place presentedTime on the rig clock")
        return
    offs = [t - h * 1e9 for t, h in pairs]
    off = statistics.median(offs)
    spread = (max(offs) - min(offs)) / 1e3
    errs = [int(s["pair_err_ns"]) / 1e3 for s in statuses if "pair_err_ns" in s]
    print(f"rig clock minus host clock: spread {spread:.1f} us over {len(pairs)} pairs "
          f"(read error median {pct(errs, 50):.1f} us)")
    dl = {int(t["f"]): int(t["dl"]) for t in ticks if t["dl"] != "0"}
    period = statistics.median([b - a for a, b in zip(sorted(dl.values()), sorted(dl.values())[1:])
                                if 0 < b - a < 100_000_000] or [16_666_667])
    shown = {}
    for p in presenteds:
        shown[int(p["d"])] = num(p["ps"])
    to_screen, from_issue, dropped, missing = [], [], 0, 0
    for p in presents:
        d, f, issued = int(p["d"]), int(p["f"]), int(p["t"])
        if d not in shown:
            missing += 1
            continue
        ps = shown[d]
        if ps == 0.0:
            dropped += 1
            continue
        at = ps * 1e9 + off
        from_issue.append((at - issued) / 1e6)
        if f in dl:
            to_screen.append((at - dl[f]) / 1e6)
    print(f"frame period: {period / 1e6:.3f} ms")
    print(f"presentedTime minus tick deadline: {dist(to_screen)}")
    if to_screen:
        print(f"  in frames: median {pct(to_screen, 50) / (period / 1e6):.2f}, "
              f"p95 {pct(to_screen, 95) / (period / 1e6):.2f}")
    print(f"presentedTime minus present call:  {dist(from_issue)}")
    print(f"presents {len(presents)}, never shown (presentedTime 0) {dropped}, "
          f"no callback seen {missing}")


def report_displays(records):
    section("Displays (D19)")
    seen = []
    for kind, kv in records:
        if kind in ("display", "edid"):
            raw = kv["raw"].split(" ", 1)[1] if kv["raw"].startswith("t=") else kv["raw"]
            if (kind, raw) not in seen:
                seen.append((kind, raw))
    if not seen:
        print("no display records")
    for kind, raw in seen:
        print(f"{kind}: {raw}")
    print("cv_latency 0/0 means macOS gave no figure; video_latency_ms=none means the EDID gave none.")


def report_timeline(records):
    section("Authority and transport timeline")
    events = []
    for kind, kv in records:
        if kind == "authority":
            events.append((int(kv["t"]), f"authority {kv['from']} -> {kv['to']}"))
        elif kind == "transport":
            v = num(kv["v"])
            events.append((int(kv["t"]), f"{kv['src']:>4} {kv['kind']}" + ("" if math.isnan(v) else f" {v:g}")))
    events.sort()
    for t, text in events[:400]:
        print(f"  {clock_text(t)}  {text}")
    if len(events) > 400:
        print(f"  ... {len(events) - 400} more")


def latest_session():
    parts = sorted(p for p in LOG_DIR.glob("sync-*.log") if PART_RE.match(p.name))
    if not parts:
        sys.exit(f"no sync rig logs in {LOG_DIR}")
    stem = PART_RE.match(parts[-1].name).group(1)
    return [p for p in parts if p.name.startswith(stem + "-p")]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("logs", nargs="*", type=Path, help="log parts of one session")
    ap.add_argument("--latest", action="store_true", help="the newest session in " + str(LOG_DIR))
    args = ap.parse_args()
    paths = latest_session() if args.latest or not args.logs else sorted(args.logs)
    print("files: " + ", ".join(str(p) for p in paths))
    records = parse(paths)
    by = defaultdict(list)
    for kind, kv in records:
        by[kind].append(kv)
    statuses = by["status"]
    if statuses:
        print(f"records dropped by the app: {statuses[-1].get('dropped', '?')}")
    print("record counts: " + ", ".join(f"{k}={len(v)}" for k, v in sorted(by.items())))

    ticks = by["tick"]
    report_ticks(ticks)
    clk_ticks, _ = report_clk(by["clk"])
    link = LinkTrack(ticks)
    samples = []
    for at, _, beat in clk_ticks:
        hit = link.at(at)
        if hit is not None:
            samples.append((at, beat, hit[0], hit[1]))
    report_phase("Link minus MIDI clock, at each clock tick's arrival (D7)", samples, None)
    report_osc(by["osc_song"], by["osc_tempo"], link)
    report_present(ticks, by["present"], by["presented"], statuses)
    report_displays(records)
    report_timeline(records)


if __name__ == "__main__":
    main()
