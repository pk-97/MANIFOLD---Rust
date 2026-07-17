"""P2 -- export.py: writes the D9 committed weights artifact + the parity
fixtures the eventual Rust port (P4) will assert against
(docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md D9, §5 P2). Reads the torch
checkpoint train.py writes and produces, both under DATA_ROOT/models/ (DATA,
gitignored -- never committed to the repo tree, per D7's "trains, never
ships" and this session's own instruction):

  - audio_event_classifier_v1.aec -- the ONLY artifact the Rust port will
    ever read; nothing about torch or Python leaks into its format. Framing
    (this module's own committed decision, since D9 names the header's
    FIELDS but not a byte framing): a 4-byte little-endian u32 giving the
    UTF-8 JSON header's length, then that many header bytes, then the
    weight blob -- every learnable tensor (skipping BatchNorm's integer
    num_batches_tracked buffer, which is training-only bookkeeping with no
    inference role) concatenated as little-endian f32, in the exact order
    the header's own "layers" list names them. Input normalization
    (mel_mean/mel_std/side_mean/side_std -- train.model.EventClassifierCNN's
    own buffers) is pulled OUT of that blob and written as the header's
    top-level "means"/"stds" fields per D9's schema, since those are what a
    reader needs before it can even run the conv stack.
  - parity_fixtures/fixture_NNNN.npz -- 32 (mel, side_features, logits)
    triples: RAW (un-normalized) inputs exactly as train/dataset.py
    extracts them, and this model's own logits (normalization happens
    INSIDE the model -- see train/model.py -- so a Rust reimplementation
    must apply the header's means/stds before its own conv stack, then
    match these logits within the D9 parity tolerance, 1e-4). Computed on
    CPU in float32 (not the training device) so the fixture is a clean
    reference independent of any MPS-specific numerics.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional

import numpy as np
import torch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.paths import DATA_ROOT  # noqa: E402
from train.dataset import PatchExample, build_dataset  # noqa: E402
from train.model import CLASS_NAMES, architecture_config, build_model  # noqa: E402
from train.train import DEFAULT_CHECKPOINT_PATH, filter_to_training_vocabulary, split_by_track  # noqa: E402

MODELS_DIR = DATA_ROOT / "models"
DEFAULT_AEC_PATH = MODELS_DIR / "audio_event_classifier_v1.aec"
DEFAULT_FIXTURES_DIR = MODELS_DIR / "parity_fixtures"
N_FIXTURES = 32
AEC_VERSION = "1"

# Buffers pulled out of the weight blob into the header's own means/stds
# fields (see module docstring) -- never written into "layers"/the blob.
_NORMALIZATION_BUFFER_NAMES = {"mel_mean", "mel_std", "side_mean", "side_std"}


def load_checkpoint(path: Path) -> Dict[str, Any]:
    return torch.load(path, map_location="cpu", weights_only=False)


def _model_from_checkpoint(checkpoint: Dict[str, Any]):
    model = build_model()
    model.load_state_dict(checkpoint["state_dict"])
    model.eval()
    return model


# ---------------------------------------------------------------------------
# .aec weights export (D9)
# ---------------------------------------------------------------------------


def export_aec(checkpoint: Dict[str, Any], out_path: Path, version: str = AEC_VERSION) -> Dict[str, Any]:
    from manifold_audio.mel_patch import HOP_MS, N_FRAMES, N_MELS

    model = _model_from_checkpoint(checkpoint)
    state = model.state_dict()

    means = {
        "mel": state["mel_mean"].numpy().astype("<f4").tolist(),
        "side": state["side_mean"].numpy().astype("<f4").tolist(),
    }
    stds = {
        "mel": state["mel_std"].numpy().astype("<f4").tolist(),
        "side": state["side_std"].numpy().astype("<f4").tolist(),
    }

    layers: List[Dict[str, Any]] = []
    blob = bytearray()
    for name, tensor in state.items():
        if name in _NORMALIZATION_BUFFER_NAMES or name.endswith("num_batches_tracked"):
            continue
        arr = tensor.detach().cpu().numpy().astype("<f4")
        layers.append({"name": name, "shape": list(arr.shape)})
        blob.extend(arr.tobytes())

    header = {
        "version": version,
        "arch": architecture_config(),
        "mels": N_MELS,
        "frames": N_FRAMES,
        "hop_ms": HOP_MS,
        "classes": list(CLASS_NAMES),
        "means": means,
        "stds": stds,
        "layers": layers,
    }
    header_bytes = json.dumps(header).encode("utf-8")

    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "wb") as f:
        f.write(len(header_bytes).to_bytes(4, "little"))
        f.write(header_bytes)
        f.write(bytes(blob))

    print(f"[export] wrote {out_path} ({len(header_bytes)}-byte header + {len(blob)}-byte weight blob, "
          f"{len(layers)} tensors)")
    return header


# ---------------------------------------------------------------------------
# Parity fixtures
# ---------------------------------------------------------------------------


def _select_fixture_examples(
    train_examples: List[PatchExample], val_examples: List[PatchExample], n: int, seed: int,
) -> List[PatchExample]:
    """Round-robin across CLASS_NAMES, preferring VAL examples (a genuine
    held-back-split reference) and falling back to TRAIN examples only for
    a class with zero val support (measured: `synth`, pinned to train by
    split_by_track's single-track rule -- see train.train's own docstring).
    Deterministic given `seed`."""
    rng = np.random.default_rng(seed)
    val_by_class: Dict[str, List[PatchExample]] = {c: [] for c in CLASS_NAMES}
    train_by_class: Dict[str, List[PatchExample]] = {c: [] for c in CLASS_NAMES}
    for ex in val_examples:
        val_by_class[ex.label].append(ex)
    for ex in train_examples:
        train_by_class[ex.label].append(ex)

    selected: List[PatchExample] = []
    class_cycle = list(CLASS_NAMES)
    i = 0
    while len(selected) < n:
        c = class_cycle[i % len(class_cycle)]
        i += 1
        pool = val_by_class[c] or train_by_class[c]
        if not pool:
            if i > len(class_cycle) * (n + 1):  # every class pool exhausted -- avoid an infinite loop
                break
            continue
        choice_idx = int(rng.integers(0, len(pool)))
        selected.append(pool.pop(choice_idx))
    return selected


def export_parity_fixtures(
    checkpoint: Dict[str, Any], out_dir: Path, n: int = N_FIXTURES, seed: Optional[int] = None,
) -> int:
    model = _model_from_checkpoint(checkpoint)
    seed = seed if seed is not None else int(checkpoint.get("history", {}).get("seed", 20260718))

    examples = filter_to_training_vocabulary(build_dataset(seed=seed))
    train_examples, val_examples = split_by_track(examples, seed=seed)
    fixtures = _select_fixture_examples(train_examples, val_examples, n=n, seed=seed)

    out_dir.mkdir(parents=True, exist_ok=True)
    with torch.no_grad():
        for idx, ex in enumerate(fixtures):
            mel_t = torch.from_numpy(ex.mel[None, None, :, :].astype(np.float32))
            side_t = torch.from_numpy(ex.side_features[None, :].astype(np.float32))
            logits = model(mel_t, side_t).numpy()[0].astype(np.float32)
            np.savez(
                out_dir / f"fixture_{idx:04d}.npz",
                mel=ex.mel.astype(np.float32),
                side_features=ex.side_features.astype(np.float32),
                logits=logits,
                label=ex.label,
                source_id=ex.source_id,
                track_id=ex.track_id,
            )
    print(f"[export] wrote {len(fixtures)} parity fixtures to {out_dir}")
    return len(fixtures)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--checkpoint", type=Path, default=DEFAULT_CHECKPOINT_PATH)
    parser.add_argument("--aec-out", type=Path, default=DEFAULT_AEC_PATH)
    parser.add_argument("--fixtures-out", type=Path, default=DEFAULT_FIXTURES_DIR)
    parser.add_argument("--n-fixtures", type=int, default=N_FIXTURES)
    args = parser.parse_args(argv)

    if not args.checkpoint.exists():
        print(f"[export] checkpoint not found: {args.checkpoint} -- run train.py first", file=sys.stderr)
        return 1

    checkpoint = load_checkpoint(args.checkpoint)
    export_aec(checkpoint, args.aec_out)
    export_parity_fixtures(checkpoint, args.fixtures_out, n=args.n_fixtures)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
