"""Frozen past-variability cues for the H8 offline attack-background trial.

These are conditional measurements, never a hard veto or source separation.
Constant additive POWER leaves novelty unchanged; adding waveforms generally
adds phase-dependent cross terms and does not obey that toy identity.
"""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import causal_features, read_audio, sha
from .kick_fusion_features import EPSILON, _rise_edges
from .kick_streaming_reference import FeatureStream
from .kick_trajectory_features import FEATURE_NAMES as TRAJECTORY_NAMES, trajectory_rows
from .kick_upper_cue import upper_features

BACKGROUND_S = .080
BANDS = ('low', 'body', 'upper')
EXTRA_NAMES = tuple(f'{b}_{kind}' for kind in ('prior_variability_novelty', 'prior_coefficient_variation')
                    for b in BANDS)
CONFIGURATIONS = ('tree39', 'tree15_background6', 'tree39_background6')
DEPENDENCIES = ('kick_attack_background.py', 'kick_attack_rejection.py',
                'kick_upper_cue.py', 'kick_fusion_features.py',
                'kick_trajectory_features.py', 'kick_streaming_reference.py')


def frozen_rule():
    return dict(hypothesis='H8', configurations=[
        dict(name='tree39', columns=39, measurements='unchanged cached39'),
        dict(name='tree15_background6', columns=21, measurements='cached15 plus6'),
        dict(name='tree39_background6', columns=45, measurements='cached39 plus6')],
        model=dict(n_estimators=64, learning_rate=.05, max_depth=3,
                   min_weight_fraction_leaf=.005, random_state=0, subsample=1.,
                   n_iter_no_change=None),
        history='ceil(.080*sample_rate/hop) prior completed fast-power hops, excluding candidate; partial startup; zero mean/std with no history',
        novelty='clip(log1p(max(max(F[c:deadline+1])-mean(prior),0)/(population_std(prior)+1e-12)),0,6)',
        variability='clip(log1p(population_std(prior)/(mean(prior)+1e-12)),0,6)',
        bands=['45–140Hz', '140–400Hz', 'sum1000–2000/2000–4000/4000–8000Hz'],
        power_follower_s=.003, background_s=BACKGROUND_S,
        invariant='Constant additive POWER leaves novelty unchanged. Waveform addition includes phase cross terms: this is not source separation.',
        preserved='Exact candidate grid; nominal40ms horizon rounded to native hops;60ms refractory; original song/class/event weights; fold-only mean/scale and z clipping8; existing nested calibration and one bracket refinement.',
        rationale='Prior variability is absent from cached39 postcandidate-to-slow-background ratios. Conditional boosted15 gains justify the tree39 control; prior ratio-balance, NNLS onset novelty and AR hard-veto failures do not establish this soft conditional measurement.',
        prohibition='No additional configurations, features, thresholds, fits on validation songs, neural/GPU work, source separation, reserved audio or integration.')


def _extra(prior, observed):
    mean = prior.mean(axis=0) if len(prior) else np.zeros(3)
    deviation = prior.std(axis=0) if len(prior) else np.zeros(3)
    excess = np.maximum(observed.max(axis=0)-mean, 0.)
    return np.clip(np.r_[np.log1p(excess/(deviation+EPSILON)),
                         np.log1p(deviation/(mean+EPSILON))], 0., 6.)


def background_rows(fast, candidates, available, sample_rate, hop):
    values = np.asarray(fast, dtype=np.float64)
    c, a = np.asarray(candidates), np.asarray(available)
    if (values.ndim != 2 or values.shape[1] != 3 or not np.isfinite(values).all()
            or np.any(values < 0) or sample_rate <= 0 or hop <= 0):
        raise ValueError('finite nonnegative three-band fast powers and positive grid required')
    if (c.ndim != 1 or a.shape != c.shape or not np.issubdtype(c.dtype, np.integer)
            or not np.issubdtype(a.dtype, np.integer) or np.any(c < 0)
            or np.any(a <= c) or np.any(a >= len(values))):
        raise ValueError('invalid candidate deadlines')
    history = math.ceil(BACKGROUND_S*sample_rate/hop)
    result = np.empty((len(c), 6))
    for j, (start, end) in enumerate(zip(c, a)):
        result[j] = _extra(values[max(0, start-history):start], values[start:end+1])
    return result


class EnvelopeStream:
    """Fixed-size fast/slow history; output is borrowed until the next call."""
    def __init__(self, sample_rate, hop):
        if sample_rate <= 0 or hop <= 0:
            raise ValueError('positive sample grid required')
        self.sample_rate, self.hop = sample_rate, hop
        self.horizon = math.ceil(.040*sample_rate/hop)
        self.history = math.ceil(BACKGROUND_S*sample_rate/hop)
        self.length = self.history+self.horizon+1
        self.ring = np.zeros((self.length, 6))
        self.ordered = np.zeros_like(self.ring)
        self.output = np.zeros(30)
        self.index = -1

    def retained_array_bytes(self):
        return self.ring.nbytes+self.ordered.nbytes+self.output.nbytes

    def push(self, fast, slow, candidate=None):
        fast, slow = np.asarray(fast), np.asarray(slow)
        if (fast.shape != (3,) or slow.shape != (3,) or not np.isfinite(fast).all()
                or not np.isfinite(slow).all() or np.any(fast < 0) or np.any(slow < 0)):
            raise ValueError('finite nonnegative three-band envelope rows required')
        self.index += 1
        self.ring[self.index % self.length] = np.r_[fast, slow]
        if candidate is None:
            return None
        if candidate < 0 or candidate != self.index-self.horizon:
            raise ValueError('candidate must use the frozen completed-hop horizon')
        start = max(0, candidate-self.history)
        count = self.index-start+1
        for j in range(count):
            self.ordered[j] = self.ring[(start+j) % self.length]
        prior_count = candidate-start
        prior = self.ordered[:prior_count, :3]
        observed = self.ordered[prior_count:count, :3]
        # Existing39: the startup background is hop0, otherwise preceding hop.
        background = self.ring[max(candidate-1, 0) % self.length, 3:]
        offsets = np.ceil(self.horizon*np.arange(1, 9)/8).astype(int)
        trajectory = np.log((observed[offsets]+EPSILON)/(background[None, :]+EPSILON))
        self.output[:24] = np.clip(trajectory, -6, 6).T.reshape(24)
        self.output[24:] = _extra(prior, observed)
        return self.output


class AttackBackgroundStream:
    """Composition with the unchanged DSP stream, retaining bounded scratch."""
    def __init__(self, sample_rate):
        self.base = FeatureStream(sample_rate)
        self.envelopes = EnvelopeStream(sample_rate, self.base.hop)
        self.hop = self.base.hop
        self.output = np.zeros(45)

    def retained_array_bytes(self):
        return self.base.retained_array_bytes()+self.envelopes.retained_array_bytes()+self.output.nbytes

    def push_hop(self, samples):
        result = self.base.push_hop(samples)
        env = self.base.envelopes
        fast = np.r_[env[:2, 0], env[2:, 0].sum()]
        slow = np.r_[env[:2, 1], env[2:, 1].sum()]
        extra = self.envelopes.push(fast, slow, result[0] if result else None)
        if result is None:
            return None
        c, a, base = result
        self.output[:15] = base
        self.output[15:39] = extra[:24]
        self.output[39:] = extra[24:]
        return c, a, self.output


def configured_features(base39, extras, configuration):
    if configuration not in CONFIGURATIONS:
        raise ValueError('configuration was not predeclared')
    if np.shape(base39)[1:] != (39,) or np.shape(extras) != (len(base39), 6):
        raise ValueError('cached39 and extra6 must have aligned rows')
    if configuration == 'tree39':
        return np.asarray(base39)
    return np.column_stack((base39[:, :15] if configuration == 'tree15_background6' else base39, extras))


def feature_names(configuration):
    if configuration not in CONFIGURATIONS:
        raise ValueError('configuration was not predeclared')
    return (TRAJECTORY_NAMES if configuration == 'tree39' else
            (TRAJECTORY_NAMES[:15] if configuration == 'tree15_background6' else TRAJECTORY_NAMES)+EXTRA_NAMES)


def cached_background(record, cache):
    """New immutable measurements, verified against every frozen trajectory row."""
    cache = Path(cache); cache.mkdir(parents=True, exist_ok=True)
    directory = Path(__file__).parent
    signature = dict(rule=frozen_rule(), source_sha256={p:sha(directory/p) for p in DEPENDENCIES},
                     original_cache=record['cache_metadata'])
    digest = hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()
    data_path, meta_path = cache/f'{digest}.npz', cache/f'{digest}.json'
    if meta_path.exists():
        meta = json.loads(meta_path.read_text())
        if meta['signature'] != signature or sha(data_path) != meta['data_sha256']:
            raise ValueError('background cache provenance differs')
        with np.load(data_path, allow_pickle=False) as z:
            extras = z['extras']
        return extras, meta
    if data_path.exists():
        raise ValueError('incomplete background cache must be inspected')
    path = Path(record['source']['audio_path'])
    if sha(path) != record['source']['audio_sha256']:
        raise ValueError('audio does not match frozen cache')
    sr, samples = read_audio(path)
    started = time.process_time()
    env, hop = causal_features(samples, sr)
    upper, upper_hop = upper_features(samples, sr)
    fast = np.column_stack((env[:, 0, 0], env[:, 1, 0], upper[:, 1:, 0].sum(axis=1)))
    slow = np.column_stack((env[:, 0, 1], env[:, 1, 1], upper[:, 1:, 1].sum(axis=1)))
    envelope_cpu = time.process_time()-started
    del samples
    if sr != record['sample_rate'] or hop != record['hop'] or hop != upper_hop:
        raise ValueError('native grid differs')
    c = np.flatnonzero(_rise_edges(env, upper)); offset = math.ceil(.040*sr/hop)
    c = c[c+offset < len(env)]; a = c+offset
    if not np.array_equal(c, record['candidates']) or not np.array_equal(a, record['available']):
        raise ValueError('frozen candidate grid differs')
    trajectory = trajectory_rows(fast, slow, c, a)
    error = float(np.max(abs(trajectory-record['features'][:, 15:39])))
    if error > 1e-12:
        raise ValueError(f'frozen trajectories differ: {error}')
    started = time.process_time(); extras = background_rows(fast, c, a, sr, hop)
    batch_cpu = time.process_time()-started
    # Full chronological envelope replay proves bounded history and alignment.
    stream = EnvelopeStream(sr, hop); replay = []; row = 0; started = time.process_time()
    for i in range(len(fast)):
        candidate = int(c[row]) if row < len(c) and int(a[row]) == i else None
        result = stream.push(fast[i], slow[i], candidate)
        if result is not None:
            replay.append(result.copy()); row += 1
    stream_cpu = time.process_time()-started
    replay = np.asarray(replay)
    stream_error = float(np.max(abs(replay-np.column_stack((trajectory, extras)))))
    if row != len(c) or stream_error > 1e-12:
        raise ValueError('bounded chronological envelope replay differs')
    np.savez(data_path, extras=extras)
    meta = dict(signature=signature, data_path=str(data_path), data_sha256=sha(data_path),
        track=record['track'], candidates=len(c), envelope_hops=len(fast),
        candidate_grid_exact=True, existing24_trajectory_max_abs_error=error,
        envelope_stream_max_abs_error=stream_error, envelope_retained_array_bytes=stream.retained_array_bytes(),
        envelope_extraction_cpu_s=envelope_cpu, extra_batch_cpu_s=batch_cpu,
        envelope_stream_cpu_s=stream_cpu,
        envelope_stream_cpu_fraction=stream_cpu/record['cache_metadata']['duration_s'],
        limitation='Offline CPU and bounded arrays only; Python temporaries allocate; no native callback deadline guarantee. Audio decode excluded from CPU measurement.')
    meta_path.write_text(json.dumps(meta, indent=2)+'\n')
    return extras, meta
