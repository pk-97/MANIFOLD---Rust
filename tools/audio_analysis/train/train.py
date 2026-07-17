"""P2/P3 R1 -- trainer for the Audio Event Classifier
(docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md D5/D7/D8, §5 P2/P3). Dev-only, never
ships (D7): trains train.model.EventClassifierCNN on train.dataset's DEV-
only corpus, prints the checkpoint gate P2 asks for, and writes a torch
checkpoint train/export.py and manifold_audio/stage1_dsp_detection.py's
classifier-labeling mode both read.

Read-back this file encodes:
  - D5: cross-entropy, small CNN (train.model), <1ms single-hit CPU
    inference is a P4 concern, not trained-for here.
  - D7: Python/PyTorch trainer, dev-only, lives in tools/audio_analysis/train/.
  - D8: NO ship bar applies at P2/P3 -- "the number is the deliverable", not
    a pass/fail. The bar (per-class F1 vs a floor) is a P3-final/P4 concern.
  - P2 brief: seeded, AdamW + cosine schedule, ~100 epochs or early stop on
    a plateau, MPS with CPU fallback, prints a held-back 10% per-class P/R/F1
    table + confusion matrix on a split done BY TRACK (a track's patches
    never straddle train/val).
  - FORBIDDEN (P2/P3 brief): no read of the two ship-candidate-reserved
    liveshow songs (train.dataset already excludes them at the source-
    loader level -- this file never re-derives or names them); no data-
    recipe edits beyond P1's pipeline except the rebalancing knobs below
    (P3's own per-round knob).

P3 ROUND 1 -- rebalancing recipe (`--recipe rebalance_v1`, now the default;
the exact P2 behavior stays reproducible via `--recipe p2`). Diagnosis this
round responds to (P2's val confusion matrix, read from the committed
checkpoint): of 788 true `kick` validation patches, 439 (55.7%) were
predicted `other` -- by far the single largest error mass in the whole
matrix, and `other`'s per-epoch cap (P2: 2x the largest drum class, drawn
uniformly, uniform CE loss) is the direct lever on it. Three knobs, dialed
together as one recipe (none touch data sources, patch geometry, or the
architecture -- P3 round-1 scope is rebalancing only):
  1. `other` per-epoch cap: 2.0x -> 1.0x the largest raw drum class. Halves
     `other`'s share of every epoch's gradient signal, aimed squarely at
     the kick->other leak above.
  2. Per-class loss weights: sqrt-inverse-frequency (not full inverse-
     frequency) over each class's RAW training-split count, normalized to
     mean 1 across the 6 classes. sqrt-damped specifically because knob 3
     already oversamples the same thin classes -- stacking full inverse-
     frequency weight on top of oversampling would double-count the
     correction for `synth` (128 raw examples) and risked an unstable
     ~25x combined up-weight; sqrt keeps the two mechanisms complementary
     rather than compounding.
  3. Per-class oversampling: every drum class's pool is redrawn (with
     replacement when needed) up to parity with the largest raw drum class
     (snare), capped at `OVERSAMPLE_MAX_MULTIPLIER`x its own raw count so a
     very thin class (synth, one pinned-to-train source track) isn't
     replayed enough times in one epoch to destabilize training on 128
     unique patches.
"""
from __future__ import annotations

import argparse
import copy
import random
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

import numpy as np
import torch
import torch.nn as nn
from sklearn.metrics import confusion_matrix, precision_recall_fscore_support

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.paths import DATA_ROOT  # noqa: E402
from manifold_audio.mel_patch import N_FRAMES, N_MELS  # noqa: E402
from train.dataset import PatchExample, build_dataset  # noqa: E402
from train.model import (  # noqa: E402
    CLASS_NAMES, N_SIDE_FEATURES, EventClassifierCNN, architecture_config, build_model, count_params,
)

DEFAULT_SEED = 20260718
DEFAULT_EPOCHS = 100
BATCH_SIZE = 64
LEARNING_RATE = 3e-4
WEIGHT_DECAY = 1e-2
VAL_FRACTION = 0.10
EARLY_STOP_PATIENCE = 15
EARLY_STOP_MIN_DELTA = 1e-4
NORMALIZATION_STD_FLOOR = 1e-6

# --- Rebalancing recipes (P3 round 1) -------------------------------------
# `p2` reproduces the exact P2 checkpoint recipe (uniform CE, no
# oversampling, `other` capped at 2x the largest raw drum class). `
# rebalance_v1` is P3 round 1's recipe -- see module docstring for the
# per-knob rationale. DEFAULT_RECIPE is rebalance_v1: P2's checkpoint
# numbers are already committed to the dev scoreboard, so overwriting the
# v1 artifacts with this round's model is the expected default; `--recipe
# p2` reruns the old recipe on demand for comparison.
RECIPE_P2 = "p2"
RECIPE_REBALANCE_V1 = "rebalance_v1"
RECIPES = (RECIPE_P2, RECIPE_REBALANCE_V1)
DEFAULT_RECIPE = RECIPE_REBALANCE_V1

OTHER_CAP_MULTIPLIER_P2 = 2.0
OTHER_CAP_MULTIPLIER_REBALANCE_V1 = 1.0
# Ceiling on how many times a thin drum class's raw pool may be replayed
# (with replacement) in one epoch under rebalance_v1 -- keeps `synth` (128
# raw examples, a single pinned-to-train source track) from being
# oversampled all the way to snare's ~2200 (a ~17x replay of the same 128
# unique patches); 6x is the compromise: real parity for classes within
# ~2-3x of the largest drum class (kick, hat, perc), a bounded rather than
# maximal correction for synth.
OVERSAMPLE_MAX_MULTIPLIER = 6.0

MODELS_DIR = DATA_ROOT / "models"
DEFAULT_CHECKPOINT_PATH = MODELS_DIR / "audio_event_classifier_v1.pt"

CLASS_TO_INDEX: Dict[str, int] = {c: i for i, c in enumerate(CLASS_NAMES)}
_DRUM_CLASSES: Tuple[str, ...] = tuple(c for c in CLASS_NAMES if c != "other")


# ---------------------------------------------------------------------------
# Reproducibility + device selection
# ---------------------------------------------------------------------------


def set_seed(seed: int) -> None:
    random.seed(seed)
    np.random.seed(seed)
    torch.manual_seed(seed)


def resolve_device() -> torch.device:
    """MPS with CPU fallback (P2 brief): probe a trivial conv forward on
    MPS before committing to it, so an environment where MPS is reported
    available but a specific op isn't supported falls back cleanly instead
    of crashing mid-epoch."""
    if torch.backends.mps.is_available():
        try:
            probe_in = torch.zeros(1, 1, 4, 4, device="mps")
            probe_conv = nn.Conv2d(1, 1, 3, padding=1).to("mps")
            probe_conv(probe_in)
            return torch.device("mps")
        except Exception as exc:  # pragma: no cover -- environment-dependent
            print(f"[train] MPS probe failed ({exc}), falling back to CPU", file=sys.stderr)
    return torch.device("cpu")


# ---------------------------------------------------------------------------
# Data prep: drop out-of-vocabulary labels, split by TRACK, tensorize
# ---------------------------------------------------------------------------


def filter_to_training_vocabulary(examples: Sequence[PatchExample]) -> List[PatchExample]:
    """Drops any example whose label isn't one of train.model.CLASS_NAMES
    (today: only `vocal`, always zero-count -- see train/model.py's own
    docstring on why vocal has no P2 head). Never silent: reports the drop
    count so an unexpected non-empty drop is visible, not swallowed."""
    kept: List[PatchExample] = []
    dropped = 0
    for ex in examples:
        if ex.label in CLASS_TO_INDEX:
            kept.append(ex)
        else:
            dropped += 1
    if dropped:
        print(
            f"[train] dropped {dropped} example(s) with label outside the P2 training "
            f"vocabulary {CLASS_NAMES} (e.g. `vocal`, deferred -- see train/model.py)",
            file=sys.stderr,
        )
    return kept


def split_by_track(
    examples: Sequence[PatchExample], seed: int, val_fraction: float = VAL_FRACTION,
) -> Tuple[List[PatchExample], List[PatchExample]]:
    """Track-level train/val split (P2 brief: "split by TRACK, not by
    patch -- patches from one track never straddle the split").

    A track that is the ONLY track carrying some class (measured: P1's
    self_render `synth` source is exactly one track, arp_16th_128bpm) is
    pinned to train -- putting it in val would zero out that class's
    TRAINING support entirely, which is strictly worse than the class
    simply having no val-side support (reported honestly in the printed
    table, not hidden).

    The remaining draw is PER-CLASS, not one global 10%-of-all-tracks
    draw, then unioned: `other` and `synth`'s truth is concentrated in a
    handful of liveshow_dev tracks, while egmd_dev alone contributes ~59
    kick/snare/hat/perc tracks -- a single global draw is dominated by
    egmd_dev's track count and (measured) can miss every liveshow_dev
    track by chance, leaving `other` with zero validation support even
    though it is the single largest class. Drawing ~val_fraction of each
    class's OWN eligible tracks (at least one, when it has more than one
    eligible track) guarantees every multi-track class gets SOME held-back
    representation; tracks naturally end up shared across classes' draws
    since one recording usually carries several classes' truth."""
    tracks_by_class: Dict[str, set] = {c: set() for c in CLASS_NAMES}
    for ex in examples:
        tracks_by_class.setdefault(ex.label, set()).add(ex.track_id)

    single_track_classes = [c for c, tracks in tracks_by_class.items() if len(tracks) == 1]
    pinned_to_train: set = set()
    for c in single_track_classes:
        pinned_to_train |= tracks_by_class[c]
        print(
            f"[train] class {c!r} has exactly one source track ({sorted(tracks_by_class[c])[0]!r}) -- "
            f"pinned to train, will report as zero-support in the validation table",
            file=sys.stderr,
        )

    rng = np.random.default_rng(seed)
    val_tracks: set = set()
    for c in sorted(CLASS_NAMES):
        if c in single_track_classes:
            continue
        eligible = sorted(t for t in tracks_by_class[c] if t not in pinned_to_train)
        if len(eligible) <= 1:
            continue  # zero or one eligible track for this class -- nothing safe to hold back
        n_c_val = max(1, int(round(len(eligible) * val_fraction)))
        shuffled = list(eligible)
        rng.shuffle(shuffled)
        val_tracks.update(shuffled[:n_c_val])

    train_examples = [ex for ex in examples if ex.track_id not in val_tracks]
    val_examples = [ex for ex in examples if ex.track_id in val_tracks]
    return train_examples, val_examples


def _stack_tensors(examples: Sequence[PatchExample]) -> Tuple[torch.Tensor, torch.Tensor, torch.Tensor]:
    if not examples:
        return (
            torch.zeros(0, 1, N_MELS, N_FRAMES, dtype=torch.float32),
            torch.zeros(0, N_SIDE_FEATURES, dtype=torch.float32),
            torch.zeros(0, dtype=torch.long),
        )
    mel = np.stack([ex.mel for ex in examples]).astype(np.float32)[:, None, :, :]
    side = np.stack([ex.side_features for ex in examples]).astype(np.float32)
    labels = np.array([CLASS_TO_INDEX[ex.label] for ex in examples], dtype=np.int64)
    return torch.from_numpy(mel), torch.from_numpy(side), torch.from_numpy(labels)


def _fit_normalization(mel: torch.Tensor, side: torch.Tensor) -> Tuple[torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor]:
    """Per-mel-band mean/std (over all training patches AND frames) + per-
    side-feature mean/std, from the TRAINING split only (never val -- see
    split_by_track's own docstring on leakage)."""
    mel_flat = mel[:, 0, :, :]  # (N, N_MELS, N_FRAMES)
    mel_mean = mel_flat.mean(dim=(0, 2))
    mel_std = mel_flat.std(dim=(0, 2)).clamp_min(NORMALIZATION_STD_FLOOR)
    side_mean = side.mean(dim=0)
    side_std = side.std(dim=0).clamp_min(NORMALIZATION_STD_FLOOR)
    return mel_mean, mel_std, side_mean, side_std


# ---------------------------------------------------------------------------
# Class-balanced per-epoch sampling
# ---------------------------------------------------------------------------


class EpochSampler:
    """`other` (the dominant class by a wide margin -- P1 measured ~11000
    raw `other` training examples vs ~2200 for the largest drum class,
    snare) is capped per epoch to a recipe-dependent multiple of the
    largest drum class's count, redrawn (without replacement, uniformly)
    each epoch from a seeded generator so the model sees a different
    `other` subset every epoch without ever letting `other` dominate the
    gradient.

    Under `rebalance_v1` (P3 round 1), every drum class's pool is ALSO
    redrawn each epoch up to parity with the largest raw drum class (with
    replacement for classes below target, capped at OVERSAMPLE_MAX_MULTIPLIER
    x that class's own raw count -- see module docstring). Under `p2`, drum
    classes are used in full every epoch, exactly as P2 shipped."""

    def __init__(self, examples: Sequence[PatchExample], seed: int, recipe: str = DEFAULT_RECIPE):
        assert recipe in RECIPES, f"unknown recipe {recipe!r}, expected one of {RECIPES}"
        self.recipe = recipe
        self.by_class: Dict[str, List[int]] = {c: [] for c in CLASS_NAMES}
        for i, ex in enumerate(examples):
            self.by_class[ex.label].append(i)
        self.rng = np.random.default_rng(seed)
        largest_drum = max((len(self.by_class[c]) for c in _DRUM_CLASSES), default=0)

        other_mult = OTHER_CAP_MULTIPLIER_REBALANCE_V1 if recipe == RECIPE_REBALANCE_V1 else OTHER_CAP_MULTIPLIER_P2
        self.other_cap = int(round(other_mult * largest_drum))

        self.oversample_targets: Dict[str, int] = {}
        if recipe == RECIPE_REBALANCE_V1:
            for c in _DRUM_CLASSES:
                raw = len(self.by_class[c])
                if raw == 0:
                    continue
                target = min(largest_drum, int(round(raw * OVERSAMPLE_MAX_MULTIPLIER)))
                if target > raw:
                    self.oversample_targets[c] = target

    def epoch_indices(self) -> np.ndarray:
        idx: List[int] = []
        for c in CLASS_NAMES:
            pool = self.by_class[c]
            if c == "other" and len(pool) > self.other_cap:
                idx.extend(self.rng.choice(pool, size=self.other_cap, replace=False).tolist())
            elif c in self.oversample_targets:
                idx.extend(self.rng.choice(pool, size=self.oversample_targets[c], replace=True).tolist())
            else:
                idx.extend(pool)
        idx_arr = np.array(idx, dtype=np.int64)
        self.rng.shuffle(idx_arr)
        return idx_arr


# ---------------------------------------------------------------------------
# Per-class loss weights (rebalance_v1 only)
# ---------------------------------------------------------------------------


def compute_class_weights(train_examples: Sequence[PatchExample], recipe: str) -> Optional[torch.Tensor]:
    """sqrt-inverse-frequency per-class weight for nn.CrossEntropyLoss,
    computed from the TRAINING split's RAW per-class counts (never val, same
    leakage discipline as _fit_normalization), normalized to mean 1 across
    the classes that actually have training support. `p2` returns None
    (uniform CE, reproduces the P2 recipe exactly). See module docstring for
    why sqrt rather than full inverse-frequency: EpochSampler's oversampling
    already corrects the same thin classes, and full inverse-frequency
    stacked on top of that oversampling was measured to over-correct
    `synth` by roughly 25x combined."""
    if recipe != RECIPE_REBALANCE_V1:
        return None
    counts = {c: 0 for c in CLASS_NAMES}
    for ex in train_examples:
        counts[ex.label] += 1
    inv_sqrt = np.array(
        [(1.0 / counts[c]) ** 0.5 if counts[c] > 0 else 0.0 for c in CLASS_NAMES], dtype=np.float64,
    )
    present = inv_sqrt[inv_sqrt > 0]
    mean = present.mean() if present.size else 1.0
    normalized = inv_sqrt / mean
    return torch.tensor(normalized, dtype=torch.float32)


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


def print_classification_report(y_true: np.ndarray, y_pred: np.ndarray, title: str) -> Dict[str, object]:
    labels = list(range(len(CLASS_NAMES)))
    precision, recall, f1, support = precision_recall_fscore_support(
        y_true, y_pred, labels=labels, zero_division=0,
    )
    print(f"[train] {title} per-class precision/recall/F1:")
    print(f"{'class':<8} {'precision':>10} {'recall':>10} {'f1':>10} {'support':>8}")
    for i, c in enumerate(CLASS_NAMES):
        print(f"{c:<8} {precision[i]:>10.4f} {recall[i]:>10.4f} {f1[i]:>10.4f} {support[i]:>8d}")

    cm = confusion_matrix(y_true, y_pred, labels=labels)
    print(f"[train] {title} confusion matrix (rows=truth, cols=pred):")
    header = " " * 9 + " ".join(f"{c:>7}" for c in CLASS_NAMES)
    print(header)
    for i, c in enumerate(CLASS_NAMES):
        row = " ".join(f"{v:>7d}" for v in cm[i])
        print(f"{c:<9}{row}")

    return {
        "precision": precision.tolist(), "recall": recall.tolist(), "f1": f1.tolist(),
        "support": support.tolist(), "confusion_matrix": cm.tolist(),
    }


# ---------------------------------------------------------------------------
# Training loop
# ---------------------------------------------------------------------------


def train_model(
    train_examples: Sequence[PatchExample],
    val_examples: Sequence[PatchExample],
    seed: int = DEFAULT_SEED,
    epochs: int = DEFAULT_EPOCHS,
    device: Optional[torch.device] = None,
    verbose: bool = True,
    recipe: str = DEFAULT_RECIPE,
) -> Tuple[EventClassifierCNN, Dict[str, object]]:
    assert recipe in RECIPES, f"unknown recipe {recipe!r}, expected one of {RECIPES}"
    device = device or resolve_device()
    set_seed(seed)

    train_mel, train_side, train_labels = _stack_tensors(train_examples)
    val_mel, val_side, val_labels = _stack_tensors(val_examples)
    mel_mean, mel_std, side_mean, side_std = _fit_normalization(train_mel, train_side)

    model = build_model()
    model.set_normalization(mel_mean, mel_std, side_mean, side_std)
    model = model.to(device)
    if verbose:
        n_params = count_params(model)
        print(f"[train] device={device} params={n_params} (ceiling 500000)")
        assert n_params <= 500_000, f"D5 ceiling exceeded: {n_params} > 500000"

    optimizer = torch.optim.AdamW(model.parameters(), lr=LEARNING_RATE, weight_decay=WEIGHT_DECAY)
    scheduler = torch.optim.lr_scheduler.CosineAnnealingLR(optimizer, T_max=max(1, epochs))
    class_weights = compute_class_weights(train_examples, recipe)
    criterion = nn.CrossEntropyLoss(weight=class_weights.to(device) if class_weights is not None else None)
    sampler = EpochSampler(train_examples, seed=seed, recipe=recipe)
    if verbose:
        other_mult = OTHER_CAP_MULTIPLIER_REBALANCE_V1 if recipe == RECIPE_REBALANCE_V1 else OTHER_CAP_MULTIPLIER_P2
        print(f"[train] recipe={recipe!r} `other` per-epoch cap: {sampler.other_cap} (largest drum class x{other_mult})")
        if sampler.oversample_targets:
            print(f"[train] oversample targets (recipe={recipe!r}): {sampler.oversample_targets}")
        if class_weights is not None:
            weights_str = ", ".join(f"{c}={w:.4f}" for c, w in zip(CLASS_NAMES, class_weights.tolist()))
            print(f"[train] per-class loss weights (sqrt-inverse-frequency, recipe={recipe!r}): {weights_str}")

    val_mel_d = val_mel.to(device)
    val_side_d = val_side.to(device)
    val_labels_d = val_labels.to(device)

    best_val_loss = float("inf")
    best_state: Optional[Dict[str, torch.Tensor]] = None
    best_epoch = -1
    patience = 0
    final_train_loss = float("nan")
    final_val_loss = float("nan")
    epochs_run = 0

    for epoch in range(epochs):
        model.train()
        idx = sampler.epoch_indices()
        epoch_loss_sum = 0.0
        n_seen = 0
        for start in range(0, len(idx), BATCH_SIZE):
            batch_idx = idx[start:start + BATCH_SIZE]
            mel_b = train_mel[batch_idx].to(device)
            side_b = train_side[batch_idx].to(device)
            label_b = train_labels[batch_idx].to(device)

            optimizer.zero_grad()
            logits = model(mel_b, side_b)
            loss = criterion(logits, label_b)
            loss.backward()
            optimizer.step()

            epoch_loss_sum += float(loss.item()) * len(batch_idx)
            n_seen += len(batch_idx)
        scheduler.step()
        train_loss = epoch_loss_sum / max(1, n_seen)

        model.eval()
        with torch.no_grad():
            if len(val_examples) > 0:
                val_logits = model(val_mel_d, val_side_d)
                val_loss = float(criterion(val_logits, val_labels_d).item())
            else:  # pragma: no cover -- val split is never empty in practice
                val_loss = train_loss

        epochs_run = epoch + 1
        final_train_loss, final_val_loss = train_loss, val_loss
        if verbose and (epoch % 10 == 0 or epoch == epochs - 1):
            print(f"[train] epoch {epoch + 1:>4}/{epochs} train_loss={train_loss:.5f} val_loss={val_loss:.5f}")

        if val_loss < best_val_loss - EARLY_STOP_MIN_DELTA:
            best_val_loss = val_loss
            best_state = copy.deepcopy(model.state_dict())
            best_epoch = epoch + 1
            patience = 0
        else:
            patience += 1
            if patience >= EARLY_STOP_PATIENCE:
                if verbose:
                    print(f"[train] early stop at epoch {epoch + 1} (no val improvement for {EARLY_STOP_PATIENCE} epochs)")
                break

    if best_state is not None:
        model.load_state_dict(best_state)
    model.eval()

    with torch.no_grad():
        val_pred = model(val_mel_d, val_side_d).argmax(dim=1).cpu().numpy() if len(val_examples) else np.array([])
    val_report = print_classification_report(val_labels.numpy(), val_pred, "validation (best checkpoint)") if verbose else {
        "precision": None, "recall": None, "f1": None, "support": None, "confusion_matrix": None,
    }

    history = {
        "seed": seed, "epochs_requested": epochs, "epochs_run": epochs_run, "best_epoch": best_epoch,
        "final_train_loss": final_train_loss, "final_val_loss": final_val_loss, "best_val_loss": best_val_loss,
        "device": str(device), "n_train": len(train_examples), "n_val": len(val_examples),
        "recipe": recipe, "other_cap": sampler.other_cap,
        "oversample_targets": dict(sampler.oversample_targets),
        "class_weights": class_weights.tolist() if class_weights is not None else None,
        "val_report": val_report,
    }
    return model, history


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_checkpoint(model: EventClassifierCNN, history: Dict[str, object]) -> Dict[str, object]:
    return {
        "state_dict": model.state_dict(),
        "classes": list(CLASS_NAMES),
        "arch": architecture_config(),
        "history": history,
    }


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--epochs", type=int, default=DEFAULT_EPOCHS)
    parser.add_argument("--out", type=Path, default=DEFAULT_CHECKPOINT_PATH)
    parser.add_argument(
        "--recipe", choices=RECIPES, default=DEFAULT_RECIPE,
        help="rebalancing recipe (P3 round 1): 'rebalance_v1' (default, this round's recipe -- "
             "see module docstring) or 'p2' (reproduces the exact P2 checkpoint recipe)",
    )
    args = parser.parse_args(argv)

    print(f"[train] building dataset (seed={args.seed}) ...", file=sys.stderr)
    t0 = time.time()
    examples = filter_to_training_vocabulary(build_dataset(seed=args.seed))
    print(f"[train] dataset built in {time.time() - t0:.1f}s, {len(examples)} examples", file=sys.stderr)

    train_examples, val_examples = split_by_track(examples, seed=args.seed)
    print(f"[train] split by track: {len(train_examples)} train / {len(val_examples)} val examples", file=sys.stderr)

    t0 = time.time()
    model, history = train_model(train_examples, val_examples, seed=args.seed, epochs=args.epochs, recipe=args.recipe)
    train_time_sec = time.time() - t0
    history["train_time_sec"] = train_time_sec
    print(f"[train] training completed in {train_time_sec:.1f}s "
          f"(epochs_run={history['epochs_run']}, best_epoch={history['best_epoch']}, "
          f"final_train_loss={history['final_train_loss']:.5f}, best_val_loss={history['best_val_loss']:.5f})")

    args.out.parent.mkdir(parents=True, exist_ok=True)
    torch.save(build_checkpoint(model, history), args.out)
    print(f"[train] wrote checkpoint: {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
