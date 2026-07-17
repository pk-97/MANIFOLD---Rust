"""Beat This! (CPJKU) beat/downbeat tracking.

Replaces madmom's DBNBeatTrackingProcessor + DBNDownBeatTrackingProcessor +
tempo-hypothesis scoring for beats, downbeats, and BPM
(docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md D1/D2, P2).

License verification (2026-07-17, read directly from the source repo, not
inferred): github.com/CPJKU/beat_this `LICENSE` file is MIT
(Copyright (c) 2024 Institute of Computational Perception, JKU Linz). The
repo's README.md `## License` section states verbatim: "The code and the
published model weights are released under the MIT license." Both code and
weights are therefore clear for this use. Pinned to `beat-this==1.1.0`
(requirements.runtime.mac.txt) — re-verify the license text on any version
bump per the doc's read-the-record-itself rule.

Weights auto-download via `torch.hub.load_state_dict_from_url` on first use
(beat_this/inference.py), cached under the process's `TORCH_HOME` (falls
back to torch's default `~/.cache/torch/hub/checkpoints/` if unset) — outside
the git repo, consistent with the other model caches in this codebase
(demucs, ADTOF, basic_pitch), none of which pin an explicit cache dir either.

CRITICAL — dbn=False is load-bearing: beat_this's internal
`Postprocessor(type="dbn")` path lazily imports madmom's DBN downbeat
processor (beat_this/model/postprocessor.py:28-31, inside the Beat This
package itself, not this codebase); the default "minimal" postprocessing
path (peak-picking on the framewise beat/downbeat activation) never touches
madmom at all. Passing dbn=True here would silently reintroduce the madmom
dependency this phase removes (D1/D2) — never do it.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from typing import Dict, List, Optional

import numpy as np

# Keyed by device string — avoids reloading the ~78 MB checkpoint per call
# when a process scores many tracks in one run (the harness's use case).
_MODEL_CACHE: Dict[str, object] = {}

CHECKPOINT = "final0"  # Beat This's default: main model, seed 0, all data except GTZAN.


@dataclass(frozen=True)
class BeatThisResult:
    beat_times: List[float]
    downbeat_times: List[float]


def _get_file2beats(device: str):
    if device not in _MODEL_CACHE:
        from beat_this.inference import File2Beats

        _MODEL_CACHE[device] = File2Beats(checkpoint_path=CHECKPOINT, device=device, dbn=False)
    return _MODEL_CACHE[device]


def estimate_beats_this(audio_path: str, device: str = "cpu") -> Optional[BeatThisResult]:
    """Run Beat This inference on a file. Returns None on any failure —
    callers fall back to the pure-DSP autocorrelation estimator (D1) and must
    stamp the result "tracker": "autocorr_fallback" in the output JSON."""
    try:
        file2beats = _get_file2beats(device)
        beats, downbeats = file2beats(audio_path)
        beat_times = sorted(
            float(t) for t in np.asarray(beats, dtype=np.float64) if np.isfinite(t) and t >= 0.0
        )
        downbeat_times = sorted(
            float(t) for t in np.asarray(downbeats, dtype=np.float64) if np.isfinite(t) and t >= 0.0
        )
        return BeatThisResult(beat_times=beat_times, downbeat_times=downbeat_times)
    except Exception as exc:
        print(f"[beat_tracking] Beat This inference failed: {exc}", file=sys.stderr)
        return None


def bpm_from_beat_times(beat_times: List[float]) -> Optional[float]:
    """Median inter-beat-interval BPM (D1: "BPM derives from inter-beat
    intervals")."""
    if len(beat_times) < 2:
        return None
    intervals = np.diff(np.asarray(beat_times, dtype=np.float64))
    intervals = intervals[intervals > 1e-6]
    if intervals.size == 0:
        return None
    return 60.0 / float(np.median(intervals))
