"""Frozen fusion features plus causal band trajectories, with immutable disk caches."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import causal_features, read_audio, sha
from .kick_fusion_bandwise import FEATURE_NAMES as BASE_NAMES, fusion_features as base_features
from .kick_upper_cue import upper_features

FEATURE_NAMES = BASE_NAMES + tuple(f'{band}_trajectory_{i}' for band in ('low', 'body', 'upper')
                                  for i in range(1, 9))
DEPENDENCIES = ('kick_trajectory_features.py', 'kick_fusion_bandwise.py',
                'kick_fusion_features.py', 'kick_attack_rejection.py',
                'kick_upper_cue.py', 'kick_tonal_experiment.py')


def trajectory_rows(fast, slow, candidates, available):
    """Eight ordered observations per band, relative to pre-candidate background.

    The native grids currently place the deadline eight hops after a candidate.
    General grids use eight equally spaced completed hops inside that horizon.
    All three denominators are frozen at the preceding hop (hop zero at startup).
    """
    if fast.shape != slow.shape or fast.ndim != 2 or fast.shape[1] != 3:
        raise ValueError('expected matching three-band power arrays')
    if len(candidates) != len(available) or np.any(available <= candidates):
        raise ValueError('invalid candidate horizon')
    offsets = np.ceil((available-candidates)[:, None] * np.arange(1, 9)[None, :] / 8).astype(int)
    indices = candidates[:, None] + offsets
    background = slow[np.maximum(candidates-1, 0)]
    values = np.log((fast[indices] + 1e-12) / (background[:, None, :] + 1e-12))
    return np.clip(values, -6, 6).transpose(0, 2, 1).reshape(len(candidates), 24)


def fusion_features(samples, sample_rate):
    candidates, available, base, hop = base_features(samples, sample_rate)
    if not len(candidates):
        return candidates, available, np.zeros((0, len(FEATURE_NAMES))), hop
    low_body, native_hop = causal_features(samples, sample_rate)
    upper, upper_hop = upper_features(samples, sample_rate)
    if native_hop != hop or upper_hop != hop:
        raise ValueError('envelope grids differ')
    fast = np.column_stack((low_body[:, 0, 0], low_body[:, 1, 0], upper[:, 1:, 0].sum(axis=1)))
    slow = np.column_stack((low_body[:, 0, 1], low_body[:, 1, 1], upper[:, 1:, 1].sum(axis=1)))
    return candidates, available, np.column_stack((base, trajectory_rows(fast, slow, candidates, available))), hop


def cached_features(audio_path, expected_audio_sha, cache_root):
    """Hash-verified, feature-only cache; labels and fitted statistics never enter it."""
    audio_path, cache_root = Path(audio_path), Path(cache_root)
    if sha(audio_path) != expected_audio_sha:
        raise ValueError('audio differs from declared source')
    directory = Path(__file__).parent
    sources = {name: sha(directory / name) for name in DEPENDENCIES}
    signature = dict(audio_sha256=expected_audio_sha, source_sha256=sources,
                     feature_names=list(FEATURE_NAMES))
    key = hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()
    cache_root.mkdir(parents=True, exist_ok=True)
    metadata_path, data_path = cache_root / f'{key}.json', cache_root / f'{key}.npz'
    if metadata_path.exists():
        metadata = json.loads(metadata_path.read_text())
        if metadata['signature'] != signature or sha(data_path) != metadata['data_sha256']:
            raise ValueError('feature cache provenance mismatch')
        with np.load(data_path, allow_pickle=False) as stored:
            arrays = {name: stored[name] for name in ('candidates', 'available', 'features')}
        return dict(metadata=metadata, **arrays, cache_hit=True)
    if data_path.exists():
        raise ValueError('incomplete cache entry; inspect it before retrying')
    sr, samples = read_audio(audio_path)
    started = time.process_time()
    candidates, available, features, hop = fusion_features(samples, sr)
    elapsed = time.process_time() - started
    np.savez(data_path, candidates=candidates, available=available, features=features)
    metadata = dict(signature=signature, sample_rate=sr, hop=hop,
                    duration_s=len(samples)/sr, feature_cpu_s=elapsed,
                    data_sha256=sha(data_path), data_path=str(data_path))
    metadata_path.write_text(json.dumps(metadata, indent=2)+'\n')
    return dict(metadata=metadata, candidates=candidates, available=available,
                features=features, cache_hit=False)
