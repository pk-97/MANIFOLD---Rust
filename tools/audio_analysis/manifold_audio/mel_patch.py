"""Log-mel patch extraction around a detected onset -- the D4 input
representation (docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md §2 D4) shared by
train/dataset.py (patch extraction for training) and this package's own
stage1_dsp_detection.py (the classifier-labeling mode added in P2). Lives
here, not in train/, so BOTH sides can import it without violating train's
own documented one-way dependency (train -> eval/manifold_audio, never the
reverse -- see train/__init__.py): this is pure signal processing (librosa
mel-spectrogram + numpy), no torch, nothing training-only.

Extracted from train/dataset.py (P1, unchanged behavior -- this is a
code-location move, not a data-recipe or numeric change; dataset.py
re-imports these same names so its own public surface and tests are
untouched).

Defaults (D4): 64 mel bands, 20Hz-16kHz, ~100ms span (~10ms pre-onset +
~90ms post), hop ~6.25ms -> a (64, 16) float32 log-mel patch.
"""
from __future__ import annotations

import librosa
import numpy as np

N_MELS = 64
MEL_FMIN_HZ = 20.0
MEL_FMAX_HZ = 16000.0
N_FRAMES = 16
PRE_ONSET_MS = 10.0
POST_ONSET_MS = 90.0
PATCH_SPAN_MS = PRE_ONSET_MS + POST_ONSET_MS  # 100.0ms, D4 default
HOP_MS = PATCH_SPAN_MS / N_FRAMES  # 6.25ms, within D4's "hop ~=6ms"
N_FFT = 1024


def hop_length(sr: int) -> int:
    return max(1, int(round(HOP_MS / 1000.0 * sr)))


def patch_segment(audio: np.ndarray, sr: int, onset_sec: float, jitter_sec: float = 0.0) -> np.ndarray:
    """Raw-sample segment long enough for exactly N_FRAMES center=False STFT
    frames at (N_FFT, hop_length), starting PRE_ONSET_MS before the
    (possibly jittered) onset. Zero-padded at track edges -- jitter or a
    near-boundary onset can run the window off either end."""
    hop = hop_length(sr)
    n_needed = N_FFT + (N_FRAMES - 1) * hop
    start_sample = int(round((onset_sec + jitter_sec - PRE_ONSET_MS / 1000.0) * sr))
    end_sample = start_sample + n_needed
    seg = np.zeros(n_needed, dtype=np.float32)
    src_start = max(0, start_sample)
    src_end = min(len(audio), end_sample)
    if src_end > src_start:
        dst_start = src_start - start_sample
        seg[dst_start: dst_start + (src_end - src_start)] = audio[src_start:src_end]
    return seg


def mel_from_segment(segment: np.ndarray, sr: int) -> np.ndarray:
    hop = hop_length(sr)
    fmax = min(MEL_FMAX_HZ, sr / 2.0)
    mel_power = librosa.feature.melspectrogram(
        y=segment.astype(np.float32), sr=sr, n_fft=N_FFT, hop_length=hop,
        center=False, n_mels=N_MELS, fmin=MEL_FMIN_HZ, fmax=fmax,
    )
    mel_db = librosa.power_to_db(mel_power, ref=1.0, amin=1e-6, top_db=None)
    if mel_db.shape[1] < N_FRAMES:
        mel_db = np.pad(mel_db, ((0, 0), (0, N_FRAMES - mel_db.shape[1])), mode="edge")
    elif mel_db.shape[1] > N_FRAMES:
        mel_db = mel_db[:, :N_FRAMES]
    return mel_db.astype(np.float32)


def extract_mel_patch(audio: np.ndarray, sr: int, onset_sec: float, jitter_sec: float = 0.0) -> np.ndarray:
    return mel_from_segment(patch_segment(audio, sr, onset_sec, jitter_sec), sr)
