"""ADTOF-pytorch drum transcription wrapper.

Wraps the ADTOF-pytorch CNN+BiLSTM model for drum onset detection
with per-class classification (kick, snare, hi-hat, tom, cymbal).

Requires adtof_pytorch to be installed — raises on import failure.
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, List, Optional

from manifold_audio.models import Event


# ADTOF MIDI pitches → MANIFOLD event types.
# LABELS_5 = [35, 38, 47, 42, 49]
_MIDI_TO_TYPE = {
    35: "kick",    # Acoustic Bass Drum
    38: "snare",   # Acoustic Snare
    47: "perc",    # Low-Mid Tom → perc
    42: "hat",     # Closed Hi-Hat
    49: "hat",     # Crash Cymbal → hat (parser already maps "cymbal" → Hat)
}

# Per-class thresholds. Order matches LABELS_5: kick, snare, tom, hihat, cymbal.
#
# kick/snare/hihat+cymbal updated 2026-07-18 to the P4 precision-pass ACCEPTED
# values (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md P4; orchestrator heldout
# acceptance read, eval/scoreboard/p4_heldout_acceptance.json) --
# threshold_factor 1.15 (kick), 1.3 (snare), 0.5 (hat, applied to BOTH the
# hihat and cymbal entries below since manifold's "hat" event type is fed by
# both) against the PRE-P4 baseline of [0.12, 0.14, 0.14, 0.18, 0.18]:
#   kick:   0.12 * 1.15 = 0.138
#   snare:  0.14 * 1.30 = 0.182
#   tom (perc): unchanged, 1.0x = 0.14
#   hihat:  0.18 * 0.50 = 0.09
#   cymbal: 0.18 * 0.50 = 0.09
# perc/synth are NOT touched by this pass (P4 round-2 verdict: only these
# three threshold picks were accepted; refractory/median_adaptive/cofire/
# shape_gate/beat_phase were all rejected for this layer, parked as
# trigger-selection-layer candidates). manifold_audio.precision_postprocessing
# .PrecisionConfig() mirrors these same five values for the eval harness.
_DEFAULT_THRESHOLDS = [0.138, 0.182, 0.14, 0.09, 0.09]

_LABEL_ORDER = ["kick", "snare", "tom", "hihat", "cymbal"]


def resolve_thresholds(thresholds: Optional[Dict[str, float]] = None) -> List[float]:
    """Effective per-class threshold list (LABELS_5 order) detect_drums_adtof
    will actually apply -- exposed so callers (analyzer.py's progress log)
    can report exactly what's in force without duplicating this merge logic
    and risking it drifting from the real one."""
    thresh_list = list(_DEFAULT_THRESHOLDS)
    if thresholds:
        for i, name in enumerate(_LABEL_ORDER):
            if name in thresholds:
                thresh_list[i] = float(thresholds[name])
    return thresh_list


def _run_adtof_inference(audio_path: str, fps: int = 100, device: str = "cpu"):
    """Shared model-load + inference helper. Returns the raw (time_steps, 5)
    class-activation array (LABELS_5 order: kick, snare, tom, hihat, cymbal),
    before any peak-picking. Factored out of detect_drums_adtof (P4,
    docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §4.2/D6) so the precision
    post-processing module (manifold_audio.precision_postprocessing) can
    operate on the same raw activations detect_drums_adtof already computed
    internally, without duplicating the model-load/inference path — both
    detect_drums_adtof and detect_drums_adtof_activations below call this,
    so detect_drums_adtof's own output is unchanged by this refactor (no
    behavior change, pure extraction)."""
    import torch
    from adtof_pytorch.model import (
        calculate_n_bins,
        create_frame_rnn_model,
        load_audio_for_model,
        load_pytorch_weights,
    )
    from adtof_pytorch import get_default_weights_path

    audio_path = str(audio_path)
    if not Path(audio_path).is_file():
        raise FileNotFoundError(f"[adtof] audio file not found: {audio_path}")

    # Load and preprocess audio using the package's own helpers.
    x = load_audio_for_model(audio_path, fps=fps)

    # Resolve device.
    if device == "cuda" and not torch.cuda.is_available():
        device = "cpu"
    if device == "mps" and not (hasattr(torch.backends, "mps") and torch.backends.mps.is_available()):
        device = "cpu"
    dev = torch.device(device)

    # Load model with bundled weights via adtof_pytorch helpers.
    n_bins = calculate_n_bins()
    model = create_frame_rnn_model(n_bins=n_bins)
    weights_path = get_default_weights_path()
    if weights_path is None or not Path(weights_path).exists():
        raise FileNotFoundError("[adtof] model weights not found in adtof_pytorch package")
    model = load_pytorch_weights(model, str(weights_path), strict=False)
    model.to(dev)
    model.eval()

    # Inference.
    with torch.no_grad():
        x = x.to(dev)
        activations = model(x).cpu().numpy()  # shape: (1, time_steps, 5)

    return activations[0]  # (time_steps, 5)


def detect_drums_adtof_activations(audio_path: str, fps: int = 100, device: str = "cpu"):
    """Raw per-class activation curve (time_steps, 5), LABELS_5 order (kick,
    snare, tom, hihat, cymbal) — the precision post-processing module's
    entry point (P4 §1(c): median-adaptive ODF baseline needs the continuous
    curve, not detect_drums_adtof's already-peak-picked events). Same
    inference path as detect_drums_adtof (via _run_adtof_inference); no
    peak-picking applied here.

    Returns
    -------
    (activations, fps) : tuple[np.ndarray, int]
    """
    return _run_adtof_inference(audio_path, fps=fps, device=device), fps


def detect_drums_adtof(
    audio_path: str,
    thresholds: Optional[Dict[str, float]] = None,
    fps: int = 100,
    device: str = "cpu",
) -> List[Event]:
    """Run ADTOF-pytorch drum transcription on an audio file.

    Parameters
    ----------
    audio_path : str
        Path to audio file (wav, mp3, etc.).
    thresholds : dict, optional
        Per-class thresholds keyed by MANIFOLD type name:
        {"kick": 0.22, "snare": 0.24, "hihat": 0.32, "tom": 0.22, "cymbal": 0.30}.
        Missing keys use defaults.
    fps : int
        Frames per second for the model (default 100).
    device : str
        PyTorch device ("cpu", "cuda", "mps").

    Returns
    -------
    list[Event]
        Sorted list of drum events.

    Raises
    ------
    ImportError
        If adtof_pytorch is not installed.
    FileNotFoundError
        If audio_path does not exist.
    """
    from adtof_pytorch.post_processing import (
        PeakPicker,
        LABELS_5,
    )

    # Build per-class threshold list in LABELS_5 order: [kick, snare, tom, hihat, cymbal].
    thresh_list = resolve_thresholds(thresholds)

    pred = _run_adtof_inference(audio_path, fps=fps, device=device)  # (time_steps, 5)

    # Peak picking with per-class thresholds.
    picker = PeakPicker(thresholds=thresh_list, fps=fps)
    peaks_list = picker.pick(pred, labels=LABELS_5, label_offset=0)

    # peaks_list is List[Dict[int, List[float]]] — one dict since single file.
    if not peaks_list:
        return []

    peaks_dict = peaks_list[0]  # Dict[midi_pitch → List[onset_time_sec]]

    # Build events, sampling activation values at onset times for confidence.
    events: List[Event] = []
    for class_idx, midi_pitch in enumerate(LABELS_5):
        event_type = _MIDI_TO_TYPE.get(midi_pitch)
        if event_type is None:
            continue

        onset_times = peaks_dict.get(midi_pitch, [])
        class_activations = pred[:, class_idx]

        for t_sec in onset_times:
            # Sample activation at onset frame for confidence.
            frame_idx = int(round(t_sec * fps))
            frame_idx = max(0, min(frame_idx, len(class_activations) - 1))
            raw_conf = float(class_activations[frame_idx])
            confidence = max(0.0, min(1.0, raw_conf))

            events.append(Event(
                type=event_type,
                time=round(float(t_sec), 4),
                confidence=round(confidence, 4),
            ))

    events.sort(key=lambda e: (e.time, e.type))
    return events
