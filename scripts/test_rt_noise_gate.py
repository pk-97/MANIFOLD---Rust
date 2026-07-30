#!/usr/bin/env python3
"""Self-test for rt_noise_gate.py — the RT temporal-stability gate.

The gate's own arithmetic is the thing a regression would silently break, so
every case here builds synthetic PNGs whose frame-to-frame delta is known by
construction and checks the reported number against it. Covers:

  1. known delta          → mean/p99.9/max match the constructed values
  2. sparse captures      → no consecutive pairs, so nothing is measured
                            (the failure mode that makes the metric meaningless)
  3. trailing window only → frames outside the consecutive block are ignored
  4. contamination        → an accel rebuild inside the window is detected,
                            one before the lookback is not
  5. median of repeats    → one wild run cannot move the verdict
  6. ceiling breach       → mean and p99.9 each fail on their own
  7. inert channel        → a channel that went dark FAILS, never passes as calm

Run: scripts/test_rt_noise_gate.py
"""

import importlib.util
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("rt_noise_gate", HERE / "rt_noise_gate.py")
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

FAILURES = []


def check(label, got, want, tol=1e-6):
    ok = abs(got - want) <= tol if isinstance(want, float) else got == want
    print(f"  {'ok  ' if ok else 'FAIL'} {label}: got {got!r} want {want!r}")
    if not ok:
        FAILURES.append(label)


def check_true(label, cond):
    print(f"  {'ok  ' if cond else 'FAIL'} {label}")
    if not cond:
        FAILURES.append(label)


def write_png(path, arr):
    Image.fromarray(arr.astype(np.uint8), mode="RGB").save(path)


def flat(level, w=16, h=8):
    return np.full((h, w, 3), level, dtype=np.uint8)


def case_known_delta(tmp):
    """A uniform step of 4 levels between every consecutive frame: mean, p99.9
    and max must all read exactly 4."""
    print("1. known uniform delta")
    d = tmp / "known"
    d.mkdir()
    for i, f in enumerate(range(100, 104)):
        write_png(d / f"chan_{f:04d}.png", flat(40 + 4 * i))
    stats, window_start = gate.measure(d)
    check("pairs", stats["chan"]["pairs"], 3)
    check("window_start", window_start, 100)
    check("mean", stats["chan"]["mean"], 4.0)
    check("p99.9", stats["chan"]["p999"], 4.0)
    check("max", stats["chan"]["max"], 4.0)
    # level is the mean brightness of the first frame of each pair: 40,44,48.
    check("level", stats["chan"]["level"], 44.0)


def case_sparse(tmp):
    """Sparse captures cannot answer the frame-to-frame question at all."""
    print("2. sparse captures yield no measurement")
    d = tmp / "sparse"
    d.mkdir()
    for f in (30, 70, 90, 150, 299):
        write_png(d / f"chan_{f:04d}.png", flat(40))
    stats, _ = gate.measure(d)
    check_true("nothing measured", stats == {})


def case_trailing_window(tmp):
    """Only the trailing consecutive block counts. Two early frames sit one
    apart with a huge delta; if they leaked in, the mean would blow up."""
    print("3. only the trailing consecutive block is measured")
    d = tmp / "window"
    d.mkdir()
    write_png(d / "chan_0010.png", flat(0))
    write_png(d / "chan_0011.png", flat(200))
    for i, f in enumerate(range(200, 204)):
        write_png(d / f"chan_{f:04d}.png", flat(40 + 2 * i))
    stats, window_start = gate.measure(d)
    check("window_start", window_start, 200)
    check("pairs", stats["chan"]["pairs"], 3)
    check("mean", stats["chan"]["mean"], 2.0)
    check("consecutive_run picks the tail", gate.consecutive_run([10, 11, 200, 201, 202, 203]),
          [200, 201, 202, 203])


def case_contamination():
    """An accel rebuild resets every accumulator. Inside the window (or within
    the lookback) the run is discarded; comfortably before it, it is fine."""
    print("4. contamination detection")
    rebuild = ("[INFO manifold_renderer] node.render_scene: RT accel structure "
               "(re)build enqueued (async, topo key 0x1, content key 0x2)")

    def cap(frame):
        return f"[rt-capture] refl_raw f={frame:04d} dim=16x8 hit=1.0 luma=0.5 sd=0.1 "

    inside = "\n".join([cap(70), cap(90), rebuild, cap(292), cap(293)])
    bad, why = gate.contaminated(inside, 292)
    check_true("rebuild inside the lookback is caught", bad)
    check_true("reason names the rebuild", why is not None and "rebuild" in why)

    early = "\n".join([rebuild, cap(70), cap(90), cap(292), cap(293)])
    bad, _ = gate.contaminated(early, 292)
    check_true("rebuild before the lookback is not flagged", not bad)

    clean = "\n".join([cap(70), cap(292), cap(293)])
    bad, _ = gate.contaminated(clean, 292)
    check_true("no rebuild, no flag", not bad)


def case_median():
    """One wild repeat must not move the verdict, and the spread must record it."""
    print("5. median of repeats absorbs one wild run")
    mk = lambda m: {"chan": {"mean": m, "p999": m * 10, "max": m * 20,
                             "level": 50.0, "pairs": 5}}
    agg = gate.median_across([mk(0.07), mk(0.08), mk(0.90)])
    check("median mean", agg["chan"]["mean"], 0.08)
    check("spread low", agg["chan"]["mean_min"], 0.07)
    check("spread high", agg["chan"]["mean_max"], 0.90)
    check("runs counted", agg["chan"]["runs"], 3)


def case_ceilings():
    """mean and p99.9 gate independently; a dark channel fails as inert."""
    print("6/7. ceiling breach and inert channel")
    baseline = {"channels": {"refl_raw": {"mean": 1.4, "p999": 55.0,
                                          "min_signal_level": 10.0}}}
    ok = {"refl_raw": {"mean": 0.7, "p999": 30.0, "max": 60.0, "level": 40.0,
                       "runs": 3, "pairs": 5}}
    check_true("within ceilings passes", gate.compare(ok, baseline)[0] == [])

    loud = dict(ok["refl_raw"], mean=2.0)
    fails = gate.compare({"refl_raw": loud}, baseline)[0]
    check_true("mean breach fails", len(fails) == 1 and "mean" in fails[0][1])

    fireflies = dict(ok["refl_raw"], p999=90.0)
    fails = gate.compare({"refl_raw": fireflies}, baseline)[0]
    check_true("p99.9 breach fails", len(fails) == 1 and "p99.9" in fails[0][1])

    # The whole reason the signal floor exists: on origin/main b10d9d94 three
    # of four back-to-back runs read every RT channel at zero while the
    # composite still rendered (BUG-mw0x). Perfectly stable, and worthless.
    dead = {"refl_raw": {"mean": 0.0, "p999": 0.0, "max": 0.0, "level": 0.0,
                         "runs": 3, "pairs": 5}}
    fails = gate.compare(dead, baseline)[0]
    check_true("inert channel fails", len(fails) == 1 and "INERT" in fails[0][1])

    missing = gate.compare({}, baseline)[0]
    check_true("vanished channel fails", len(missing) == 1 and "absent" in missing[0][1])

    # A dead run is discarded before the median, not averaged into it: its 0.0
    # delta would pull the verdict toward fake-calm. moments legitimately reads
    # 0.879 on a live run, so the test is "literally black", not "small".
    check("dead run detected", gate.dead_channels(dead), ["refl_raw"])
    live = {"refl_raw": dict(ok["refl_raw"]), "moments": dict(ok["refl_raw"], level=0.879)}
    check("live run kept", gate.dead_channels(live), [])


def main():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        case_known_delta(tmp)
        case_sparse(tmp)
        case_trailing_window(tmp)
    case_contamination()
    case_median()
    case_ceilings()
    if FAILURES:
        print(f"\nFAILED: {len(FAILURES)} check(s): {', '.join(FAILURES)}")
        return 1
    print("\nall checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
