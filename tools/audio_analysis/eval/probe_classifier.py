"""Audio Event Classifier -- the DEV-side probe (pre-exam sanity check).

Same scoring machinery as the D8 exam (eval/exam_classifier.py -- this
module imports its corpus builders, arms, and table verbatim) over DEV
slices ONLY: liveshow dev songs (DENSE_IN_WINDOW, kick/snare/hat) + a
capped E-GMD dev subset (dense, kick/snare/hat/perc). HELDOUT IS FORBIDDEN
here by construction: the only fixtures this file can name are split ==
"dev" ones, same structural-guard convention as eval/sweep_p4.py's module
docstring. The heldout read is eval/exam_classifier.py's, behind its
explicit flag, spent once per ship candidate (D8).

Use it before any exam spend: if the probe's dev table doesn't reproduce
the dev numbers the candidate was accepted on (or the model file is
missing/mispointed), the exam would measure the wrong thing -- fix that
here, where re-runs are free.

Graceful degradation: if the classifier weights file is absent from the
data store, the probe runs the ADTOF/DSP arm only and says so loudly --
an ADTOF-only dev table is still a useful sanity read (and proves the
harness end-to-end) when no candidate exists yet.

Usage:
    python -m eval.probe_classifier [--max-egmd 4] [--out probe.json]
"""

from __future__ import annotations

import argparse
import datetime as dt
import sys
from pathlib import Path
from typing import List, Optional

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.beat_scoring import LIVESHOW_SONG_FIXTURES  # noqa: E402
from eval.exam_classifier import (  # noqa: E402
    BAR_CLASSES,
    DEFAULT_WEIGHTS_REL,
    DRUM_CLASSES,
    SLICE_EGMD,
    SLICE_LIVESHOW,
    build_egmd_tracks,
    build_liveshow_tracks,
    print_table,
    run_scoreboard,
    write_scoreboard,
)
from eval.paths import DATA_ROOT  # noqa: E402

# DEV liveshow songs only -- the heldout pair (liveshow_stagnate,
# liveshow_basalt) is never referenced anywhere in this file.
DEV_LIVESHOW_FIXTURES = [fx for fx in LIVESHOW_SONG_FIXTURES if fx["split"] == "dev"]

# E-GMD dev is 59 rows; the probe is a sanity check, not the bake-off, so
# it caps the subset by default (ADTOF inference dominates runtime). The
# cap takes the manifest's FIRST N dev rows -- a fixed, documented subset,
# not a per-run sample.
DEFAULT_MAX_EGMD = 4


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data-root", type=Path, default=DATA_ROOT,
                        help="eval data store (default: eval.paths.DATA_ROOT, the main checkout's store)")
    parser.add_argument("--classifier-weights", type=Path, default=None,
                        help=f"classifier checkpoint (default: <data-root>/{DEFAULT_WEIGHTS_REL})")
    parser.add_argument("--max-egmd", type=int, default=DEFAULT_MAX_EGMD,
                        help=f"cap on E-GMD dev rows scored (default {DEFAULT_MAX_EGMD}; 0 = all dev rows)")
    parser.add_argument("--out", type=Path, default=None,
                        help="optional JSON scoreboard artifact path (stdout table is always printed)")
    args = parser.parse_args(argv)

    weights = args.classifier_weights or (args.data_root / DEFAULT_WEIGHTS_REL)
    adtof_only = not weights.exists()
    if adtof_only:
        print(f"[probe_classifier] classifier weights MISSING: {weights} -- "
              f"running the ADTOF/DSP arm only (degraded mode).", file=sys.stderr)

    max_egmd = args.max_egmd if args.max_egmd > 0 else None
    print("[probe_classifier] building DEV corpus (liveshow dev + E-GMD dev) ...", file=sys.stderr)
    groups = {
        SLICE_LIVESHOW: build_liveshow_tracks(DEV_LIVESHOW_FIXTURES, args.data_root),
        SLICE_EGMD: build_egmd_tracks("dev", args.data_root, max_tracks=max_egmd),
    }
    classes_by_group = {SLICE_LIVESHOW: BAR_CLASSES, SLICE_EGMD: DRUM_CLASSES}

    scoreboard = run_scoreboard(
        groups, classes_by_group, None if adtof_only else str(weights), adtof_only=adtof_only,
        group_titles={SLICE_LIVESHOW: "liveshow dev", SLICE_EGMD: "E-GMD dev"},
    )
    print_table(scoreboard, adtof_only=adtof_only)

    if args.out is not None:
        payload = {
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "kind": "audio_event_classifier dev probe (HELDOUT FORBIDDEN here)",
            "adtof_only": adtof_only,
            "classifier_weights": None if adtof_only else str(weights),
            "corpus_summary": {
                label: [{"id": t.id, "domain": t.domain, "truth_type": t.truth_type} for t in tracks]
                for label, tracks in groups.items()
            },
            **scoreboard,
        }
        write_scoreboard(args.out, payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
