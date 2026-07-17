"""D5 -- the Audio Event Classifier CNN (docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md
§2 D5, §5 P2). Plain PyTorch, no external model libraries. Input: a (1, 64,
16) log-mel patch (D4, manifold_audio.mel_patch's own geometry) + 6 side-
feature scalars (train.dataset.SIDE_FEATURE_NAMES order); output: logits
over CLASS_NAMES below.

CLASS_NAMES is D3's 7-class vocabulary MINUS `vocal`: P1 measured that
every one of the 27 liveshow vocal truth labels lives in the two songs
reserved for the ship-candidate read (train/sources.toml's own note on the
liveshow_dev source), so P2 has ZERO permitted vocal training data --
training a 7th head against no examples isn't a modeling choice, there is
nothing to fit. Surfaced as its own read-back item at the P2 checkpoint
(docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md §7 Deferred, added at P1). Fine
labels stay in train/dataset.py's own D3 vocabulary untouched (D3: "fine
labels exist in the data; merging at output preserves the option") -- this
module's 6-class vocabulary is the P2 modeling decision, not a rewrite of
what dataset.py extracts. If dev-side vocal labels ever exist, reviving the
7th head is a CLASS_NAMES + head-width change here, nothing upstream.

Architecture: 4 conv blocks (Conv2d -> BatchNorm2d -> ReLU, first 3 blocks
end in a 2x2 max-pool) -> global average pool -> concatenate the 6 side
features -> a 2-layer dense head. Input normalization (per-mel-band mean/
std, per-side-feature mean/std) lives INSIDE the model as registered
buffers (mel_mean/mel_std/side_mean/side_std) rather than as a separate
preprocessing step, so train.py, export.py, and any future Rust port (P4,
D9) all apply the identical transform by construction -- there is exactly
one place normalization happens. Buffers default to mean=0/std=1 (a no-op)
until train.py fits and sets them from the training split.

Rejected (D5): transformers (no benefit at this scale, this is a ~100k-
param model on ~100ms of audio); waveform-domain nets (a spectrogram CNN is
the robust default for short-sound classification, and D4 already commits
to the log-mel input). No in-repo precedent exists for a trained model --
this is the repo's first one; the inference precedent is set at P4 (D9),
deliberately, not here.
"""
from __future__ import annotations

from typing import Dict, Tuple

import torch
import torch.nn as nn

from manifold_audio.mel_patch import N_FRAMES, N_MELS

CLASS_NAMES: Tuple[str, ...] = ("kick", "snare", "hat", "perc", "synth", "other")
N_SIDE_FEATURES = 6

# Conv channel widths per block (D5: 3-4 conv blocks). Chosen to keep the
# model comfortably under the 500k-param ceiling while giving the 6-way
# head real capacity -- see count_params() below for the measured total,
# printed by train.py at the start of every run (D5's own gate: "print the
# count").
_CONV_CHANNELS: Tuple[int, int, int, int] = (24, 48, 72, 96)
_KERNEL_SIZE = 3
_HIDDEN_DIM = 96
_DROPOUT = 0.2


def _conv_pool_block(in_ch: int, out_ch: int) -> nn.Sequential:
    return nn.Sequential(
        nn.Conv2d(in_ch, out_ch, kernel_size=_KERNEL_SIZE, padding=1),
        nn.BatchNorm2d(out_ch),
        nn.ReLU(inplace=True),
        nn.MaxPool2d(2),
    )


class EventClassifierCNN(nn.Module):
    """(mel, side_features) -> logits over CLASS_NAMES.

    mel: (B, 1, N_MELS, N_FRAMES) float32, RAW log-mel dB (not pre-
    normalized by the caller -- this module normalizes internally using its
    own mel_mean/mel_std buffers).
    side_features: (B, N_SIDE_FEATURES) float32, RAW physical units
    (train.dataset.SIDE_FEATURE_NAMES order), normalized internally the
    same way via side_mean/side_std.
    """

    def __init__(self, n_classes: int = len(CLASS_NAMES), n_side_features: int = N_SIDE_FEATURES):
        super().__init__()
        c1, c2, c3, c4 = _CONV_CHANNELS
        self.block1 = _conv_pool_block(1, c1)
        self.block2 = _conv_pool_block(c1, c2)
        self.block3 = _conv_pool_block(c2, c3)
        # 4th block: no further pool. (N_MELS, N_FRAMES) = (64, 16) survives
        # three /2 pools as (8, 2); a 4th pool would collapse a dimension to
        # 1 for no capacity benefit.
        self.block4 = nn.Sequential(
            nn.Conv2d(c3, c4, kernel_size=_KERNEL_SIZE, padding=1),
            nn.BatchNorm2d(c4),
            nn.ReLU(inplace=True),
        )
        self.global_pool = nn.AdaptiveAvgPool2d(1)
        self.head = nn.Sequential(
            nn.Linear(c4 + n_side_features, _HIDDEN_DIM),
            nn.ReLU(inplace=True),
            nn.Dropout(_DROPOUT),
            nn.Linear(_HIDDEN_DIM, n_classes),
        )

        # Input normalization buffers -- see module docstring. Included in
        # state_dict() (and therefore every checkpoint) automatically since
        # these are registered buffers, not plain attributes.
        self.register_buffer("mel_mean", torch.zeros(N_MELS))
        self.register_buffer("mel_std", torch.ones(N_MELS))
        self.register_buffer("side_mean", torch.zeros(n_side_features))
        self.register_buffer("side_std", torch.ones(n_side_features))

    def set_normalization(
        self, mel_mean: torch.Tensor, mel_std: torch.Tensor, side_mean: torch.Tensor, side_std: torch.Tensor,
    ) -> None:
        """Fit-once call from train.py, BEFORE training starts, using
        training-split statistics only (never validation, to avoid leaking
        split information into the input transform). std is floored at a
        small epsilon by the caller to avoid a divide-by-zero on a
        constant side-feature column."""
        self.mel_mean.copy_(mel_mean)
        self.mel_std.copy_(mel_std)
        self.side_mean.copy_(side_mean)
        self.side_std.copy_(side_std)

    def forward(self, mel: torch.Tensor, side_features: torch.Tensor) -> torch.Tensor:
        mel_norm = (mel - self.mel_mean.view(1, 1, -1, 1)) / self.mel_std.view(1, 1, -1, 1)
        side_norm = (side_features - self.side_mean.view(1, -1)) / self.side_std.view(1, -1)

        x = self.block1(mel_norm)
        x = self.block2(x)
        x = self.block3(x)
        x = self.block4(x)
        x = self.global_pool(x).flatten(1)  # (B, c4)
        x = torch.cat([x, side_norm], dim=1)
        return self.head(x)


def build_model() -> EventClassifierCNN:
    return EventClassifierCNN()


def count_params(model: nn.Module) -> int:
    return sum(p.numel() for p in model.parameters())


def architecture_config() -> Dict[str, object]:
    """Everything a from-scratch reconstruction (Python or, eventually,
    Rust -- P4/D9) needs to rebuild this exact module graph, independent of
    the state_dict's own tensor shapes. Written verbatim into export.py's
    .aec header under "arch"."""
    return {
        "kind": "cnn_v1",
        "conv_channels": list(_CONV_CHANNELS),
        "kernel_size": _KERNEL_SIZE,
        "hidden_dim": _HIDDEN_DIM,
        "dropout": _DROPOUT,
        "n_side_features": N_SIDE_FEATURES,
        "n_classes": len(CLASS_NAMES),
        "mels": N_MELS,
        "frames": N_FRAMES,
    }


if __name__ == "__main__":
    m = build_model()
    n_params = count_params(m)
    print(f"[model] EventClassifierCNN param count: {n_params} (ceiling: 500000)")
    assert n_params <= 500_000, f"D5 ceiling exceeded: {n_params} > 500000"
    mel = torch.zeros(2, 1, N_MELS, N_FRAMES)
    side = torch.zeros(2, N_SIDE_FEATURES)
    out = m(mel, side)
    print(f"[model] forward output shape: {tuple(out.shape)} (expected (2, {len(CLASS_NAMES)}))")
