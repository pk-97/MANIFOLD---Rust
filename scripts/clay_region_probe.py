#!/usr/bin/env python3
"""Region-mean convergence probe for the Solid (clay) render-mode gate
(SCENE_RENDER_MODE_DESIGN.md P2). Compares the SAME two named pixel rects
across a Rendered and a Solid headless render of one multi-material scene
(produced by `graph-tool render` of the clay-probe graphs):

  --rendered   the mode-0 capture    --solid   the mode-1 capture

Oracle, two legs, both must pass (exit 0):
  separation: the two rects' mean RGB distance in the RENDERED capture is
              >= --min-separation — the probe regions genuinely carry two
              different albedos (if they start equal, convergence proves
              nothing).
  converge:   the same two rects' mean RGB max-channel distance in the
              SOLID capture is <= --max-converge — Solid collapsed both
              materials to the one clay color, lighting intact (the rects
              see symmetric lighting, so the clay means must match; the
              tolerance absorbs antialiasing/tonemap shimmer only).

Stdout is one JSON object (same contract as rt_region_probe.py).
{"mean_a_rendered", "mean_b_rendered", "separation", "mean_a_solid",
 "mean_b_solid", "convergence", "separation_pass", "converge_pass"}
"""
import argparse
import json
import sys

import numpy as np
from PIL import Image


def rgb_mean(path: str, rect: tuple[int, int, int, int]) -> np.ndarray:
    x0, y0, x1, y1 = rect
    a = np.asarray(Image.open(path).convert("RGB")).astype(float) / 255.0
    r = a[y0:y1, x0:x1]
    if r.size == 0:
        sys.exit(f"empty rect {rect} for {path}")
    return r.reshape(-1, 3).mean(axis=0)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rendered", required=True, help="mode-0 (Rendered) capture")
    ap.add_argument("--solid", required=True, help="mode-1 (Solid) capture")
    ap.add_argument("--rect-a", required=True, help="first object region x0,y0,x1,y1")
    ap.add_argument("--rect-b", required=True, help="second object region x0,y0,x1,y1")
    ap.add_argument("--min-separation", type=float, default=0.2,
                    help="minimum mean RGB distance between the two regions "
                         "in the Rendered capture (0..1)")
    ap.add_argument("--max-converge", type=float, default=0.05,
                    help="maximum per-channel mean RGB distance between the "
                         "two regions in the Solid capture (0..1)")
    args = ap.parse_args()
    rect_a = tuple(int(v) for v in args.rect_a.split(","))
    rect_b = tuple(int(v) for v in args.rect_b.split(","))

    a_r = rgb_mean(args.rendered, rect_a)
    b_r = rgb_mean(args.rendered, rect_b)
    separation = float(np.linalg.norm(a_r - b_r))
    a_s = rgb_mean(args.solid, rect_a)
    b_s = rgb_mean(args.solid, rect_b)
    convergence = float(np.abs(a_s - b_s).max())

    separation_pass = separation >= args.min_separation
    converge_pass = convergence <= args.max_converge
    print(json.dumps({
        "mean_a_rendered": [round(v, 4) for v in a_r],
        "mean_b_rendered": [round(v, 4) for v in b_r],
        "separation": round(separation, 4),
        "mean_a_solid": [round(v, 4) for v in a_s],
        "mean_b_solid": [round(v, 4) for v in b_s],
        "convergence": round(convergence, 4),
        "separation_pass": separation_pass,
        "converge_pass": converge_pass,
    }))
    return 0 if (separation_pass and converge_pass) else 1


if __name__ == "__main__":
    sys.exit(main())
