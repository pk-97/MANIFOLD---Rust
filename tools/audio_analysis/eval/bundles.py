"""AnalysisBundle — content-addressed cache of model intermediates (D7).

Key = SHA-256 of source audio bytes + per-stage pipeline version + model
stamps. Value = stems paths, beat/downbeat times, ADTOF activations,
basic_pitch note candidates, per-stem envelopes — stored as one JSON
sidecar (scalars/lists/paths) plus an optional .npz (arrays).

Default location `~/Library/Caches/Manifold/analysis-bundles/`, overridable
via `cache_dir`. Two consumers per D7: the app (re-detect / cross-project
reuse become cache reads — not wired in P1) and this harness (tuning
iterates over cached arrays without re-running models). This EXTENDS the
existing demucs config-hash cache (manifold_audio/external_tools.py:244) to
every model stage — it does not replace the app-side per-clip event cache
(audio_clip_detection.rs:152), which stays untouched.

A bundle regenerated under different stamps never silently mixes into a
comparison (D11): load_bundle returns None (cache miss) rather than a
stale/mismatched bundle whenever the stamps on disk don't match what the
caller asked for.
"""

from __future__ import annotations

import hashlib
import json
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional

import numpy as np

DEFAULT_CACHE_DIR = Path.home() / "Library" / "Caches" / "Manifold" / "analysis-bundles"


@dataclass(frozen=True)
class BundleStamp:
    """What produced this bundle — the thing D11 says must match exactly or
    the bundle is a miss, never a silent partial reuse."""

    pipeline_version: str
    models: Dict[str, str] = field(default_factory=dict)  # e.g. {"beat_tracker": "madmom-0.16.1"}
    seed: int = 0

    def to_dict(self) -> Dict[str, Any]:
        return {"pipeline_version": self.pipeline_version, "models": dict(self.models), "seed": self.seed}

    @staticmethod
    def from_dict(d: Dict[str, Any]) -> "BundleStamp":
        return BundleStamp(
            pipeline_version=d["pipeline_version"],
            models=dict(d.get("models", {})),
            seed=int(d.get("seed", 0)),
        )


@dataclass
class AnalysisBundle:
    """One track's worth of cached model intermediates. `arrays` holds
    anything that should live in the .npz sidecar (envelopes, activation
    matrices); `scalar` holds the JSON-safe rest (events, beat times, bpm,
    stem paths)."""

    content_hash: str
    stamp: BundleStamp
    scalar: Dict[str, Any] = field(default_factory=dict)
    arrays: Dict[str, np.ndarray] = field(default_factory=dict)

    def save(self, cache_dir: Path) -> Path:
        cache_dir.mkdir(parents=True, exist_ok=True)
        base = cache_dir / self._filename_stem()
        json_path = base.with_suffix(".json")
        npz_path = base.with_suffix(".npz")
        payload = {
            "content_hash": self.content_hash,
            "stamp": self.stamp.to_dict(),
            "scalar": self.scalar,
            "has_arrays": bool(self.arrays),
        }
        # NOTE: np.savez_compressed silently APPENDS ".npz" to any filename
        # that doesn't already end in it — a tmp path like "*.npz.tmp" would
        # actually get written to "*.npz.tmp.npz", and the os.replace below
        # would then raise FileNotFoundError. Keep the temp file's name
        # ending in ".npz" (differentiated by a ".tmp" infix instead).
        tmp_json = json_path.with_name(json_path.stem + ".tmp.json")
        tmp_json.write_text(json.dumps(payload, indent=2))
        os.replace(tmp_json, json_path)
        if self.arrays:
            tmp_npz = npz_path.with_name(npz_path.stem + ".tmp.npz")
            np.savez_compressed(tmp_npz, **self.arrays)
            os.replace(tmp_npz, npz_path)
        elif npz_path.exists():
            npz_path.unlink()
        return json_path

    def _filename_stem(self) -> str:
        stamp_hash = hashlib.sha256(
            json.dumps(self.stamp.to_dict(), sort_keys=True).encode("utf-8")
        ).hexdigest()[:12]
        return f"{self.content_hash}_{stamp_hash}"


def content_hash_for_audio(audio_path: Path) -> str:
    """SHA-256 of the raw file bytes. Deliberately the file, not the decoded
    signal, so the hash is cheap (no decode needed just to check the cache)
    and format-sensitive (a wav and its mp3 re-encode are different cache
    entries, which is correct — D14 says decoders differ)."""
    h = hashlib.sha256()
    with open(audio_path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _filename_stem_for(content_hash: str, stamp: BundleStamp) -> str:
    stamp_hash = hashlib.sha256(json.dumps(stamp.to_dict(), sort_keys=True).encode("utf-8")).hexdigest()[:12]
    return f"{content_hash}_{stamp_hash}"


def load_bundle(content_hash: str, stamp: BundleStamp, cache_dir: Path = DEFAULT_CACHE_DIR) -> Optional[AnalysisBundle]:
    """Returns the cached bundle only if content_hash AND every stamp field
    match exactly. Any mismatch (missing file, different pipeline_version,
    different model version, different seed) is a cache miss — never a
    partial/best-effort reuse (D11)."""
    stem = _filename_stem_for(content_hash, stamp)
    json_path = cache_dir / f"{stem}.json"
    if not json_path.exists():
        return None
    payload = json.loads(json_path.read_text())
    if payload.get("content_hash") != content_hash:
        return None
    on_disk_stamp = BundleStamp.from_dict(payload["stamp"])
    if on_disk_stamp != stamp:
        return None
    arrays: Dict[str, np.ndarray] = {}
    if payload.get("has_arrays"):
        npz_path = cache_dir / f"{stem}.npz"
        if npz_path.exists():
            with np.load(npz_path) as data:
                arrays = {k: data[k] for k in data.files}
    return AnalysisBundle(content_hash=content_hash, stamp=on_disk_stamp, scalar=payload["scalar"], arrays=arrays)


def build_or_load_bundle(
    audio_path: Path,
    stamp: BundleStamp,
    compute_fn,
    cache_dir: Path = DEFAULT_CACHE_DIR,
    force: bool = False,
) -> AnalysisBundle:
    """The one entry point callers should use. compute_fn(audio_path) -> (scalar_dict, arrays_dict).

    force=True bypasses the cache read (still writes the fresh result back,
    so a forced recompute updates the cache for next time — this is how you
    invalidate a bundle after a genuine model/pipeline change, not by hand-
    deleting cache files).
    """
    chash = content_hash_for_audio(audio_path)
    if not force:
        cached = load_bundle(chash, stamp, cache_dir)
        if cached is not None:
            return cached
    scalar, arrays = compute_fn(audio_path)
    bundle = AnalysisBundle(content_hash=chash, stamp=stamp, scalar=scalar, arrays=arrays)
    bundle.save(cache_dir)
    return bundle


def list_cached_stamps(cache_dir: Path = DEFAULT_CACHE_DIR) -> List[str]:
    """Diagnostic helper: every distinct pipeline_version stamped into the
    cache dir, for spotting stale bundles left over from an old pipeline
    version during manual cache inspection."""
    if not cache_dir.exists():
        return []
    versions = set()
    for json_path in cache_dir.glob("*.json"):
        try:
            payload = json.loads(json_path.read_text())
            versions.add(payload["stamp"]["pipeline_version"])
        except Exception:
            continue
    return sorted(versions)
