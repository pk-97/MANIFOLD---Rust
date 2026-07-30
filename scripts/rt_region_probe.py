#!/usr/bin/env python3
"""Region-mean red-minus-green probe for the multi-bounce GI gate
(RAYTRACING_DESIGN.md section 11, MB-B). Compares the SAME pixel rect across
two renders of tools/rt_prototype/compare/RtBleed.json:

  --a  the 1-bounce capture (MB-A commit)   --b  the 2-bounce capture (MB-B)

Oracle: the second bounce reaches the probe region only via the RED wall, so
it raises R more than G there; any direct-emitter leak is white (R==G) and
cancels. Two legs, both must pass (exit 0):
  control: |R-G| of A within --max-control  (no tint at 1 bounce)
  bleed:   (R-G of B) - (R-G of A) >= --min-delta

Stdout is one JSON object (consumed as a workflow `transform` artifact):
{"rg_a", "rg_b", "delta", "control_pass", "bleed_pass", "pin_threshold"}
pin_threshold = max(0.006, delta/2) — the in-repo regression test's floor.
"""
import argparse
import json
import sys

import numpy as np
from PIL import Image


def rg_mean(path: str, rect: tuple[int, int, int, int]) -> tuple[float, float]:
    x0, y0, x1, y1 = rect
    a = np.asarray(Image.open(path).convert("RGB")).astype(float) / 255.0
    r = a[y0:y1, x0:x1]
    if r.size == 0:
        sys.exit(f"empty rect {rect} for {path}")
    return float(r[:, :, 0].mean()), float(r[:, :, 1].mean())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", required=True, help="1-bounce render (control)")
    ap.add_argument("--b", required=True, help="2-bounce render")
    ap.add_argument("--rect", required=True, help="x0,y0,x1,y1")
    ap.add_argument("--min-delta", type=float, required=True)
    ap.add_argument("--max-control", type=float, required=True)
    args = ap.parse_args()
    rect = tuple(int(v) for v in args.rect.split(","))
    ra, ga = rg_mean(args.a, rect)
    rb, gb = rg_mean(args.b, rect)
    rg_a, rg_b = ra - ga, rb - gb
    delta = rg_b - rg_a
    control_pass = abs(rg_a) <= args.max_control
    bleed_pass = delta >= args.min_delta
    print(json.dumps({
        "rg_a": round(rg_a, 5),
        "rg_b": round(rg_b, 5),
        "delta": round(delta, 5),
        "control_pass": control_pass,
        "bleed_pass": bleed_pass,
        "pin_threshold": round(max(0.006, delta / 2.0), 5),
    }))
    return 0 if (control_pass and bleed_pass) else 1


if __name__ == "__main__":
    sys.exit(main())
