#!/usr/bin/env python3
"""Non-zero-pixel probe for the Points render-mode gate
(SCENE_RENDER_MODE_DESIGN.md P3). Counts pixels ABOVE BACKGROUND inside a
stated rect of a headless points capture (produced by `graph-tool render`
of a points-probe graph):

  --points  the mode-3 (Points) capture

Oracle, one leg, must pass (exit 0):
  nonzero: the rect contains >= --min-nonzero pixels whose brightest
           channel exceeds --threshold — a dense procedural mesh (grid_mesh
           128x128) drawn as points fills the region with dots; a
           triangle-only render of the same graph leaves it empty, and the
           mode-0 capture is the visual reference for Peter, not the gate.

Stdout is one JSON object (same contract as clay_region_probe.py).
{"nonzero", "region", "threshold", "nonzero_pass"}
"""
import argparse
import json
import sys

import numpy as np
from PIL import Image


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--points", required=True, help="mode-3 (Points) capture")
    ap.add_argument("--rect", required=True, help="probe region x0,y0,x1,y1")
    ap.add_argument("--threshold", type=int, default=16,
                    help="a pixel counts as lit when its max channel exceeds this")
    ap.add_argument("--min-nonzero", type=int, default=1000,
                    help="minimum lit pixels for the probe to pass")
    args = ap.parse_args()
    rect = tuple(int(v) for v in args.rect.split(","))

    x0, y0, x1, y1 = rect
    a = np.asarray(Image.open(args.points).convert("RGB"))
    r = a[y0:y1, x0:x1]
    if r.size == 0:
        sys.exit(f"empty rect {rect} for {args.points}")
    lit = int((r.max(axis=2) > args.threshold).sum())

    print(json.dumps({
        "nonzero": lit,
        "region": rect,
        "threshold": args.threshold,
        "nonzero_pass": lit >= args.min_nonzero,
    }))
    return 0 if lit >= args.min_nonzero else 1


if __name__ == "__main__":
    sys.exit(main())
