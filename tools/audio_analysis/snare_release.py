#!/usr/bin/env python3
"""Snare model release: fit the final net, then export the model file and the Rust parity goldens.

Usage:
  snare_release.py train                 the final net on every labelled song; cutoff from the held-out run
  snare_release.py export --out DIR      DIR/assets/snare_model.mkick and DIR/tests/fixtures/snare/clip_<name>.mkick

The recipe (BUG-9ngk8.2 (Snare+clap), 2026-10-10): snare_cands candidates (1-8 kHz rise >= 4.5 dB), one CNN on
the kick net's 64-band spectrum with a 190 ms slice ending 40 ms after emission (four time segments) and a third
input channel of per-band noisiness (detector_channels.flatness), cutoff by pooled F1 over the held-out run,
60 ms refractory. No trees, no stage: on this net they added nothing.

The held-out score comes from the leave-one-song-out run that must exist first:
  SNARE_FLAT=1 SNARE_PAST_MS=190 eval/snare_net.py 0      (GOAL/snare_net_s0_snare_past_ms190_snare_flat1.npz)
Goldens are the Python reference run on each clip alone, the stream starting at the clip's first sample; every
clip starts with silence, so no candidate's slice or baseline reaches before the start.
"""
from __future__ import annotations

import os
import sys

# Net input shape: 190 ms slice (95 frames, four time segments), third channel slot carries noisiness.
RECIPE_ENV = dict(KICK_GOAL_NN_PAST_MS='190', KICK_GOAL_NN_PERC='1', KICK_GOAL_NN_AHEAD_MS='0', SNARE_FLAT='1',
                  SNARE_PAST_MS='190', KICK_GOAL_TRUTH='v3', KICK_GOAL_MORE='1', KICK_GOAL_TRIGGER='1',
                  KICK_GOAL_WIP='1', KICK_GOAL_PROJECT='1', KICK_GOAL_RECALL='1')
for _k, _v in RECIPE_ENV.items():
    if os.environ.get(_k, _v) != _v:
        sys.exit(f'{_k}={os.environ[_k]} is not the release recipe ({_v})')
    os.environ[_k] = _v
for _k in ('KICK_GOAL_NN_BANDS', 'KICK_GOAL_NN_STEREO', 'SNARE_AHEAD_MS'):
    if os.environ.get(_k):
        sys.exit(f'{_k} is set; the release recipe uses the defaults')
for _k in ('OMP_NUM_THREADS', 'OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'MKL_NUM_THREADS'):
    os.environ.setdefault(_k, '1')

import argparse  # noqa: E402
import datetime  # noqa: E402
import json  # noqa: E402
import pickle  # noqa: E402
import subprocess  # noqa: E402
import time  # noqa: E402
from pathlib import Path  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402
from scipy.signal import butter  # noqa: E402

from tools.audio_analysis import kick_container  # noqa: E402
from tools.audio_analysis.eval import kick_goal_nn as nnm  # noqa: E402
from tools.audio_analysis.eval.detector_channels import N_FFT, flatness  # noqa: E402
from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL  # noqa: E402
from tools.audio_analysis.eval.snare_cands import DEADLINE, HOP, LOOKBACK, REFRACTORY, candidates  # noqa: E402
from tools.audio_analysis.eval.snare_net import AHEAD, REFR, choose, fires  # noqa: E402

FINAL = GOAL / 'snare_final' / 'final.pkl'
HELD = GOAL / 'snare_net_s0_snare_past_ms190_snare_flat1.npz'
RECIPE_VERSION = 1
SR = 48000
RISE_DB = 4.5
CAND_BAND = (1000, 8000)
CAND_WINDOW_S = .005
FLAT_CLIP = (-20.0, 0.0)
FIXTURES = Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio')
END_MARGIN_S = .060  # past the last emission: the 40 ms look-ahead + a frame + the jitter guard
LEAD_SILENCE_S = .5
CLIPS = {
    'dense': dict(parts=[('silence', LEAD_SILENCE_S), ('tears_140bpm', 0.0, 0.0), ('apricots_128bpm', 0.0, 0.0)], target_s=18.0),
    'quiet_start': dict(parts=[('silence', LEAD_SILENCE_S + .25), ('bad_guy_128bpm', 0.0, 2.0)], target_s=12.0),
}


def git_id():
    sha = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True,
                         check=True).stdout.strip()
    dirty = subprocess.run(['git', '-C', str(ROOT), 'status', '--porcelain', '--', 'tools/audio_analysis'],
                           capture_output=True, text=True, check=True).stdout.strip()
    return sha + ('-dirty' if dirty else '')


# ---------------------------------------------------------------- train

def labelled_songs(g, dev):
    """(Song per labelled song, per-song scoring metadata) built exactly as snare_net builds them."""
    from tools.audio_analysis.eval.detector_channels import flat_cache
    from tools.audio_analysis.eval.detector_songs import audio, rate, spec
    from tools.audio_analysis.eval.snare_net import rows
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    data, meta = {}, {}
    for t, s in lab.items():
        sr = rate(g, t)
        cand, avail = candidates(audio(g, t), sr, RISE_DB)
        onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
        mask, y, hard = rows(s, onset)
        data[t] = nnm.Song(spec(g, t), onset, emit + AHEAD, mask, y, t, hard, (flat_cache(g, t),)).to(dev)
        meta[t] = (s, emit, avail, sr)
    return lab, data, meta


def train():
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    t0 = time.time()
    g = Goal(mode='v3')
    dev = nnm.device()
    lab, data, meta = labelled_songs(g, dev)
    held = dict(np.load(HELD))
    if set(held) != set(lab):
        raise SystemExit(f'{HELD.name} covers other songs than snare_labels.json; rerun snare_net.py first')
    cutoff = choose([(meta[u][0], meta[u][1], held[u], meta[u][2], meta[u][3]) for u in lab if not lab[u]['positives_only']])
    net = nnm.train([data[t] for t in lab], 0)
    train_id = f'snare-{datetime.date.today().isoformat()}-{git_id()}'
    final = dict(train_id=train_id, cutoff=float(cutoff), refractory_s=REFR, songs=sorted(lab),
                 net=dict(state_dict={k: v.detach().cpu().numpy().astype(np.float32) for k, v in net.state_dict().items()},
                          bands=nnm.BANDS, past_ms=nnm.PAST_MS, ahead_ms=round(AHEAD * 1000), channels=nnm.CHANNELS))
    FINAL.parent.mkdir(parents=True, exist_ok=True)
    with open(FINAL, 'wb') as fh:
        pickle.dump(final, fh)
    print(f'wrote {FINAL}: train_id {train_id}, {len(lab)} songs, cutoff {cutoff:.4f}, {time.time() - t0:.0f} s', flush=True)


# ---------------------------------------------------------------- reference

def net_module(final):
    import torch
    n = final['net']
    assert (n['bands'], n['past_ms'], n['channels']) == (nnm.BANDS, nnm.PAST_MS, nnm.CHANNELS), 'net input settings differ'
    net = nnm.Net()
    net.load_state_dict({k: torch.from_numpy(np.asarray(v, np.float32)) for k, v in n['state_dict'].items()})
    return net.eval()


def reference(x, final, net):
    """Every golden array for audio x (f64, 48 kHz), the stream starting at x[0]."""
    import torch
    from tools.audio_analysis.eval.snare_cands import band_hops
    os.environ['KICK_GOAL_NN_DEVICE'] = 'cpu'
    cand, avail = candidates(x, SR, RISE_DB)
    onset_s, emit_s = (cand + 1) * HOP / SR, (avail + 1) * HOP / SR
    r = dict(cand_db=band_hops(x, SR, *CAND_BAND), cand_hop=cand, avail_hop=avail, onset_s=onset_s, emit_s=emit_s)
    r['net_spec'] = nnm.spectrum(x, SR).astype(np.float32)
    r['flat'] = flatness(x, SR).astype(np.float32)
    k = len(cand)
    song = nnm.Song(r['net_spec'], onset_s, emit_s + AHEAD, np.zeros(k, bool), np.zeros(k), extra=(r['flat'],))
    with torch.no_grad():
        logits = net(song.slices(np.arange(k))).numpy().astype(np.float64) if k else np.zeros(0)
    r['net_logit'] = logits
    r['net_p'] = 1 / (1 + np.exp(-logits))
    r['fires'] = avail[fires(r['net_p'], avail, final['cutoff'], SR)] if k else np.zeros(0, int)
    return r


# ---------------------------------------------------------------- clips

def quantise(x):
    return np.clip(np.round(x * 32768.0), -32768, 32767).astype(np.int16)


def build_clip(spec):
    """(i16 audio, description). The end sits in a candidate-free gap END_MARGIN_S past the last emission."""
    parts, desc = [], []
    for p in spec['parts']:
        if p[0] == 'silence':
            parts.append(np.zeros(int(round(p[1] * SR))))
            desc.append(f'{p[1]:.2f} s digital silence')
            continue
        name, start, fade = p
        x = read_audio(FIXTURES / name / 'mix.wav', SR)[1][int(round(start * SR)):].copy()
        if fade:
            n = int(round(fade * SR))
            x[:n] *= np.linspace(0.0, 1.0, n)
        parts.append(x)
        desc.append(f'{name}/mix.wav from {start:.2f} s' + (f' with a {fade:.1f} s linear fade-in' if fade else ''))
    a16 = quantise(np.concatenate(parts))
    x = a16 / 32768.0
    _, avail = candidates(x, SR, RISE_DB)
    emit = (avail + 1) * HOP / SR
    for a, b in zip(emit, np.append(emit[1:], len(x) / SR)):
        end_s = a + END_MARGIN_S
        if end_s >= spec['target_s'] and end_s < b:
            end = int(np.ceil(end_s * SR)) + 37  # odd, so the clip never ends on a hop boundary
            if end / SR < b:
                return a16[:end], ' + '.join(desc) + f', cut at sample {end} ({end / SR:.4f} s)'
    raise RuntimeError(f'no candidate-free gap after {spec["target_s"]} s')


# ---------------------------------------------------------------- export

def model_entries(final):
    """Every number the Rust detector needs."""
    n = final['net']
    f64, i64 = np.float64, np.int64
    frame_hop = int(round(nnm.FRAME_S * SR))
    i = np.arange(2_000_000)
    assert np.array_equal(np.round(i * nnm.FRAME_S * SR).astype(np.int64), i * frame_hop)
    freqs = np.fft.rfftfreq(N_FFT, 1 / SR)
    lo, cnt = [], []
    for fc in nnm.CENTRES:
        idx = np.flatnonzero((freqs >= fc * (1 - nnm.HALF_BW)) & (freqs <= fc * (1 + nnm.HALF_BW)))
        if len(idx) < 3:
            idx = np.sort(np.argsort(np.abs(freqs - fc))[:3])
        assert np.array_equal(idx, np.arange(idx[0], idx[0] + len(idx))), 'flatness bands must be contiguous bins'
        lo.append(idx[0])
        cnt.append(len(idx))
    out = {
        'recipe_version': np.array(RECIPE_VERSION, np.int32),
        'train_id': kick_container.text(final['train_id']),
        'cutoff': np.array(final['cutoff'], f64),
        'refractory_s': np.array(final['refractory_s'], f64),
        'sample_rate': np.array(SR, i64),
        'cand.sos': butter(4, CAND_BAND, btype='band', fs=SR, output='sos').astype(f64),
        'cand.window': np.array(int(CAND_WINDOW_S * SR), i64),
        'cand.hop': np.array(HOP, i64),
        'cand.rise_db': np.array(RISE_DB, f64),
        'cand.lookback': np.array(LOOKBACK, i64),
        'cand.refractory': np.array(REFRACTORY, i64),
        'cand.deadline': np.array(DEADLINE, i64),
        'flat.n_fft': np.array(N_FFT, i64),
        'flat.window': np.hanning(N_FFT).astype(f64),
        'flat.band_lo': np.array(lo, i64),
        'flat.band_n': np.array(cnt, i64),
        'flat.clip_db': np.array(FLAT_CLIP, f64),
        'net.sos': np.stack([butter(2, [fc * (1 - nnm.HALF_BW), fc * (1 + nnm.HALF_BW)], btype='bandpass', fs=SR, output='sos')
                             for fc in nnm.CENTRES]).astype(f64),
        'net.k': np.array([max(int(.002 * SR), int(2 * SR / fc)) for fc in nnm.CENTRES], i64),
        'net.hop': np.array(HOP, i64),
        'net.sample_rate': np.array(SR, i64),
        'net.frame_s': np.array(nnm.FRAME_S, f64),
        'net.frame_hop': np.array(frame_hop, i64),
        'net.slice': np.array(nnm.SLICE, i64),
        'net.tbins': np.array(nnm.TBINS, i64),
        'net.channels': np.array(nnm.CHANNELS, i64),
        'net.pre_s': np.array(nnm.PRE_S, f64),
        'net.span': np.array(int(nnm.PRE_S / nnm.FRAME_S), i64),
        'net.ahead_s': np.array(AHEAD, f64),
    }
    assert n['ahead_ms'] == round(AHEAD * 1000)
    for name, w in n['state_dict'].items():
        out[f'net.{name}'] = np.ascontiguousarray(np.asarray(w, np.float32))
    return out


def export(out_dir):
    t0 = time.time()
    with open(FINAL, 'rb') as fh:
        final = pickle.load(fh)
    print(f'final model {final["train_id"]}, cutoff {final["cutoff"]:.4f}', flush=True)
    net = net_module(final)
    gold = Path(out_dir) / 'tests' / 'fixtures' / 'snare'
    gold.mkdir(parents=True, exist_ok=True)
    i64 = np.int64
    for name, spec in CLIPS.items():
        a16, desc = build_clip(spec)
        x = a16 / 32768.0
        r = reference(x, final, net)
        dur = len(x) / SR
        if len(r['emit_s']) and r['emit_s'][-1] > dur - END_MARGIN_S:
            raise SystemExit(f'clip {name}: last emission too close to the end')
        if len(r['onset_s']) and r['onset_s'][0] - nnm.PRE_S < 0 or len(r['emit_s']) and r['emit_s'][0] + AHEAD - nnm.SLICE * nnm.FRAME_S < 0:
            raise SystemExit(f'clip {name}: the first candidate reaches before the clip start')
        entries = {
            'train_id': kick_container.text(final['train_id']), 'sample_rate': np.array(SR, i64), 'hop': np.array(HOP, i64),
            'audio_i16': a16, 'cand_db': r['cand_db'].astype(np.float64),
            'cand_hop': r['cand_hop'].astype(i64), 'avail_hop': r['avail_hop'].astype(i64),
            'onset_s': r['onset_s'].astype(np.float64), 'emit_s': r['emit_s'].astype(np.float64),
            'net_spec': r['net_spec'], 'flat': r['flat'],
            'net_logit': r['net_logit'].astype(np.float64), 'net_p': r['net_p'].astype(np.float64),
            'fires': np.asarray(r['fires']).astype(i64),
        }
        path = gold / f'clip_{name}.mkick'
        kick_container.write(path, entries)
        back = kick_container.read(path)
        bad = [k for k, v in entries.items() if not (np.array_equal(back[k], v) and np.all(np.isfinite(v)))]
        if bad:
            raise SystemExit(f'clip {name}: entries not finite or not round-tripping: {bad}')
        print(f'clip {name}: {desc}; {dur:.3f} s, {len(r["cand_hop"])} candidates, {len(r["fires"])} fires -> {path}', flush=True)
    model = model_entries(final)
    path = Path(out_dir) / 'assets' / 'snare_model.mkick'
    path.parent.mkdir(parents=True, exist_ok=True)
    kick_container.write(path, model)
    print(f'model: {len(model)} entries -> {path} ({path.stat().st_size} bytes); total {time.time() - t0:.0f} s', flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    sub.add_parser('train')
    e = sub.add_parser('export')
    e.add_argument('--out', required=True)
    a = ap.parse_args()
    train() if a.cmd == 'train' else export(a.out)


if __name__ == '__main__':
    main()
