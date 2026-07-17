"""D7 AnalysisBundle cache tests: content-addressing, stamp-mismatch = miss
(never a silent partial reuse), array round-trip."""

from __future__ import annotations

import numpy as np
import pytest

from eval.bundles import (
    AnalysisBundle,
    BundleStamp,
    build_or_load_bundle,
    content_hash_for_audio,
    load_bundle,
)


@pytest.fixture
def wav_file(tmp_path):
    import wave

    path = tmp_path / "test.wav"
    with wave.open(str(path), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(44100)
        wf.writeframes((np.zeros(1000, dtype=np.int16)).tobytes())
    return path


def test_content_hash_is_stable(wav_file):
    h1 = content_hash_for_audio(wav_file)
    h2 = content_hash_for_audio(wav_file)
    assert h1 == h2
    assert len(h1) == 64  # sha256 hex


def test_build_or_load_computes_once_then_caches(wav_file, tmp_path):
    cache_dir = tmp_path / "cache"
    stamp = BundleStamp(pipeline_version="test-v1", models={"onset": "fake-1.0"}, seed=0)
    calls = {"n": 0}

    def compute_fn(path):
        calls["n"] += 1
        return {"bpm": 120.0}, {"envelope": np.array([1.0, 2.0, 3.0])}

    b1 = build_or_load_bundle(wav_file, stamp, compute_fn, cache_dir=cache_dir)
    b2 = build_or_load_bundle(wav_file, stamp, compute_fn, cache_dir=cache_dir)
    assert calls["n"] == 1  # second call was a cache hit
    assert b1.scalar["bpm"] == 120.0
    assert b2.scalar["bpm"] == 120.0
    np.testing.assert_array_equal(b2.arrays["envelope"], np.array([1.0, 2.0, 3.0]))


def test_stamp_mismatch_is_a_miss_never_a_partial_reuse(wav_file, tmp_path):
    cache_dir = tmp_path / "cache"
    stamp_a = BundleStamp(pipeline_version="v1", models={"onset": "a"}, seed=0)
    stamp_b = BundleStamp(pipeline_version="v2", models={"onset": "b"}, seed=0)
    calls = {"n": 0}

    def compute_fn(path):
        calls["n"] += 1
        return {"bpm": float(calls["n"])}, {}

    build_or_load_bundle(wav_file, stamp_a, compute_fn, cache_dir=cache_dir)
    b2 = build_or_load_bundle(wav_file, stamp_b, compute_fn, cache_dir=cache_dir)
    assert calls["n"] == 2  # different stamp -> recompute, not a stale reuse
    assert b2.scalar["bpm"] == 2.0


def test_load_bundle_returns_none_when_absent(tmp_path):
    stamp = BundleStamp(pipeline_version="v1")
    assert load_bundle("deadbeef" * 8, stamp, cache_dir=tmp_path) is None


def test_force_recompute_updates_the_cache(wav_file, tmp_path):
    cache_dir = tmp_path / "cache"
    stamp = BundleStamp(pipeline_version="v1")
    calls = {"n": 0}

    def compute_fn(path):
        calls["n"] += 1
        return {"value": calls["n"]}, {}

    build_or_load_bundle(wav_file, stamp, compute_fn, cache_dir=cache_dir)
    b2 = build_or_load_bundle(wav_file, stamp, compute_fn, cache_dir=cache_dir, force=True)
    assert calls["n"] == 2
    assert b2.scalar["value"] == 2

    b3 = build_or_load_bundle(wav_file, stamp, compute_fn, cache_dir=cache_dir)
    assert calls["n"] == 2  # b3 reads the force-refreshed cache, no 3rd compute
    assert b3.scalar["value"] == 2
