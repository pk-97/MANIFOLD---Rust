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

# Per-class thresholds (lowered from ADTOF defaults for higher sensitivity).
# Order matches LABELS_5: kick, snare, tom, hihat, cymbal.
_DEFAULT_THRESHOLDS = [0.12, 0.14, 0.14, 0.18, 0.18]


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
    import torch
    import numpy as np
    from adtof_pytorch.model import (
        calculate_n_bins,
        create_frame_rnn_model,
        load_audio_for_model,
        load_pytorch_weights,
    )
    from adtof_pytorch.post_processing import (
        PeakPicker,
        LABELS_5,
    )
    from adtof_pytorch import get_default_weights_path

    audio_path = str(audio_path)
    if not Path(audio_path).is_file():
        raise FileNotFoundError(f"[adtof] audio file not found: {audio_path}")

    # Build per-class threshold list in LABELS_5 order: [kick, snare, tom, hihat, cymbal].
    thresh_list = list(_DEFAULT_THRESHOLDS)
    if thresholds:
        _label_order = ["kick", "snare", "tom", "hihat", "cymbal"]
        for i, name in enumerate(_label_order):
            if name in thresholds:
                thresh_list[i] = float(thresholds[name])

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

    pred = activations[0]  # (time_steps, 5)

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
