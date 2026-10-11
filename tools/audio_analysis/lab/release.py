#!/usr/bin/env python3
"""Detector releases (docs/DETECTOR_RUNTIME_DESIGN.md section 3): one tool for any lab-built detector.

Usage:
  release.py train CONFIG [--seed S] [--nets N]      final models from a saved lab run (seeds S..S+N-1); a release record
  release.py train CONFIG --provisional [--nets N]   CPU, 1 epoch, 3 songs, trees max_iter 5, cuts and cutoff from its coverage clips: throwaway
  release.py convert-v1 kick|snare                   the shipped v1 model as a release record: same train_id, same numbers
  release.py export TRAIN_ID --set NAME [--ship] [--out ROOT]
        writes crates/manifold-audio/tests/fixtures/detectors/NAME/{model,clip_*}.mkick under ROOT (default this
        checkout); --ship also writes crates/manifold-audio/assets/<family>_model.mkick, byte-identical to the model
  release.py parts                                   runtime_parts.json against the lab registries; exits 1 on a mismatch

A record lives in ~/.cache/manifold/detector-lab/releases/<train_id>/ (final.pkl, release.json). The reference that
writes goldens is the lab's own parts (components finders, measures and frame builders, kick_goal_nn, the fitted
trees, models.combine, stage.inputs and stage.gated, detector_eval.fires), run on each clip alone with f32 spectra
and frames, never the float16 training caches. Each process pins one record's net env (kick_goal_nn reads it at
import), so a process works on records of one net shape.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import pickle
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402

from tools.audio_analysis import kick_container  # noqa: E402
from tools.audio_analysis.lab import config, recipe  # noqa: E402

RELEASES = Path.home() / '.cache/manifold/detector-lab/releases'
SETS = Path('crates/manifold-audio/tests/fixtures/detectors')
ASSETS = Path('crates/manifold-audio/assets')
PARTS = ROOT / SETS / 'runtime_parts.json'
CONTRACT = 2
RATE, HOP = 48000, 256
CLIP = 1e-6  # every logit clip (models.combine, the D17 net average)
GOAL = Path.home() / '.cache/manifold/kick-goal-2026-10-10'
# The shipped v1 models and the lab config each was built as (provenance only).
V1 = {'kick': (GOAL / 'final' / 'final.pkl', 'kick_shipped_recipe'),
      'snare': (GOAL / 'snare_final' / 'final.pkl', 'snare_v1')}
V1_KICK_TREES = ['f15', 'tail3', 'tmpl3', 'prof32', 'low16']
# The fixture audio is not in git: the main checkout's path.
FIXTURES = Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio')
LEAD_SILENCE_S = .5
TEARS, APRICOTS = ('tears_140bpm', 0.0, 0.0), ('apricots_128bpm', 0.0, 0.0)
QUIET = [('silence', .75), ('bad_guy_128bpm', 0.0, 2.0)]
PROVISIONAL_SONGS = 3


# ---------------------------------------------------------------- records

def parts():
    return json.loads(PARTS.read_text())


def use(cfg_dict, name='release'):
    """The config as an Experiment, its net env pinned before any eval/ import; a process whose net module already
    read another env exits."""
    # Checking a config imports the parts, and with them kick_goal_nn: the env goes first.
    d = config.migrate(cfg_dict)
    recipe.apply(config.Experiment(name=name, family=d['family'], candidates=d['candidates'], models=d['models'],
                                   combine=d['combine'], stage=d['stage'], sampling=d['sampling'],
                                   train=tuple(d['train']), seeds=(), folds=int(d['folds'])).net_env())
    cfg = config.build(name, cfg_dict)
    nn = sys.modules.get('tools.audio_analysis.eval.kick_goal_nn')
    want = cfg.net['past_ms'] if cfg.net else 150
    if nn is not None and nn.PAST_MS != want:
        sys.exit(f'kick_goal_nn read past_ms {nn.PAST_MS} in this process; this record needs {want}: run it in its own process')
    return cfg


def summary(rec):
    """release.json: the record without its fitted objects."""
    net = rec['net'] and {k: v for k, v in rec['net'].items() if k != 'state_dicts'} | dict(count=len(rec['net']['state_dicts']))
    return dict(
        contract=rec['contract'], family=rec['family'], train_id=rec['train_id'], config=rec['config'], run_id=rec['run_id'],
        seed=rec['seed'], finder=rec['finder'], net=net,
        trees=rec['trees'] and dict(measures=rec['trees']['measures']),
        fitted={m: {k: list(np.shape(v)) for k, v in p.items()} for m, p in rec['fitted'].items()},
        combine=rec['combine'],
        stage=rec['stage'] and {k: v for k, v in rec['stage'].items() if k != 'model'},
        cutoff=rec['cutoff'], refractory_s=rec['refractory_s'], songs=rec['songs'], score=rec['score'], clips=rec['clips'])


def save(rec):
    d = RELEASES / rec['train_id']
    d.mkdir(parents=True, exist_ok=True)
    tmp = d / f'final.pkl.{os.getpid()}.tmp'
    with open(tmp, 'wb') as fh:
        pickle.dump(rec, fh)
    os.replace(tmp, d / 'final.pkl')
    (d / 'release.json').write_text(json.dumps(summary(rec), indent=1, default=float) + '\n')
    print(f'release {rec["train_id"]} -> {d}', flush=True)


def load(train_id):
    path = RELEASES / train_id / 'final.pkl'
    if not path.exists():
        sys.exit(f'no release {train_id} ({path})')
    with open(path, 'rb') as fh:
        rec = pickle.load(fh)
    if rec.get('contract') != CONTRACT:
        sys.exit(f'{train_id}: not a contract {CONTRACT} release record')
    return rec


def open_release(train_id):
    """The record, with its net env pinned in this process."""
    rec = load(train_id)
    use(rec['config'], train_id)
    return rec


def measures_of(rec):
    """Every event measure the model reads (trees, stage shape, stage level), first use order: Spec::measures()."""
    out = list(rec['trees']['measures']) if rec['trees'] else []
    if rec['stage']:
        out += list(rec['stage']['shape']) + ([rec['stage']['level'][0]] if rec['stage']['level'] else [])
    return list(dict.fromkeys(out))


def git_id():
    sha = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True,
                         check=True).stdout.strip()
    dirty = subprocess.run(['git', '-C', str(ROOT), 'status', '--porcelain', '--', 'tools/audio_analysis'],
                           capture_output=True, text=True, check=True).stdout.strip()
    return sha + ('-dirty' if dirty else '')


# ---------------------------------------------------------------- parts

def refuse_undeclared(cfg, pts=None):
    """A config part the app cannot run stops the release before anything trains."""
    pts = pts or parts()
    bad = []
    kind = cfg.candidates.split(':')[0]
    if kind not in pts['finders']:
        bad.append(f'finder {cfg.candidates}')
    used = list(cfg.models['trees']['measures']) if 'trees' in cfg.models else []
    if cfg.stage:
        used += list(cfg.stage['shape']) + ([cfg.stage['level'][0]] if cfg.stage['level'] else [])
        if len(cfg.stage['windows_s']) > pts['stage']['max_windows']:
            bad.append(f'{len(cfg.stage["windows_s"])} stage windows')
    bad += [f'measure {m}' for m in dict.fromkeys(used) if m not in pts['measures']]
    if 'f15' in used and kind != 'kick':
        bad.append('f15 without the kick finder')
    if cfg.net:
        bad += [f'channel {c}' for c in cfg.net['channels'] if c not in pts['channels']]
    if cfg.combine not in pts['combines']:
        bad.append(f'combine {cfg.combine}')
    if 'trees' in cfg.models and cfg.models['trees']['templates'] is not None:
        bad.append('trees.templates (lab-fitted templates at release are Deferred)')
    if bad:
        raise SystemExit(f'{cfg.name}: the app does not run {", ".join(bad)} ({PARTS.relative_to(ROOT)})')


def synthetic_clip(sr=RATE):
    """4.5 s with kick- and snare-like hits after 0.5 s of silence: enough for every finder to find candidates."""
    rng = np.random.default_rng(0)
    x = np.zeros(int(4.5 * sr))
    t = np.arange(int(.25 * sr)) / sr
    kick = np.sin(2 * np.pi * (50 + 100 * np.exp(-t * 30)) * t) * np.exp(-t * 12)
    snare = rng.normal(0, .3, len(t)) * np.exp(-t * 25)
    for k, at in enumerate(np.arange(.5, 4.0, .25)):
        i = int(at * sr)
        x[i:i + len(t)] += .8 * kick if k % 2 == 0 else snare
    return x + rng.normal(0, 1e-3, len(x))


def check_parts(pts=None):
    """[problems]: every declared part exists in the lab registries with its declared width."""
    from tools.audio_analysis.lab import components
    from tools.audio_analysis.lab.config import COMBINES
    from tools.audio_analysis.lab.export.measures import deadline
    pts = pts or parts()
    out = []
    x = synthetic_clip()
    cands = {}
    for f in pts['finders']:
        rule = 'kick' if f == 'kick' else f'{f}:4.5'
        if f not in components.FINDERS or f not in components.AUDIO_FINDERS:
            out.append(f'finder {f}: not an audio-only lab finder')
            continue
        cands[f] = components.audio_candidates(rule, f, x, RATE)
        if not len(cands[f].cand):
            out.append(f'finder {f}: no candidates on the synthetic clip')
    c = cands.get('kick')
    for m, width in pts['measures'].items():
        if m in components.FITTED:
            span = deadline('kick') + 1
            params = dict(templates=np.random.default_rng(1).uniform(0, 1, (2, span, 32)))
            got = components.FITTED[m]['apply'](params, x, RATE, c)
        elif m in components.MEASURES:
            got = components.MEASURES[m](x, RATE, c)
        else:
            out.append(f'measure {m}: not in the lab registries')
            continue
        if np.shape(got) != (len(c.cand), width):
            out.append(f'measure {m}: shape {np.shape(got)}, declared width {width}')
    for ch in pts['channels']:
        if ch not in components.FRAME_MEASURES or ch not in components.FRAME_BUILDERS:
            out.append(f'channel {ch}: not a lab frame measure with a builder')
            continue
        from tools.audio_analysis.eval import kick_goal_nn as nnm
        got = components.FRAME_BUILDERS[ch](x, RATE)
        if got.shape[1] != nnm.BANDS or got.dtype != np.float32:
            out.append(f'channel {ch}: {got.dtype}{got.shape}, want float32 (frames, {nnm.BANDS})')
    out += [f'combine {cb}: not a lab combine' for cb in pts['combines'] if cb not in COMBINES]
    if pts['nets'] != dict(min_count=1) or pts['stage'] != dict(max_windows=4, level=True, gate=True):
        out.append(f'nets {pts["nets"]} / stage {pts["stage"]}: not the contract limits')
    return out


# ---------------------------------------------------------------- reference pipeline

def net_modules(rec):
    """Each net with its weights, on the CPU, in eval mode."""
    import torch

    from tools.audio_analysis.eval import kick_goal_nn as nnm
    n = rec['net']
    assert n['past_ms'] == nnm.PAST_MS, 'net input settings differ from the record'
    out = []
    for sd in n['state_dicts']:
        net = nnm.Net(2 + len(n['channels']))
        net.load_state_dict({k: torch.from_numpy(np.asarray(v, np.float32)) for k, v in sd.items()})
        out.append(net.eval())
    return out


def net_logits(net, song):
    """(n,) f32 logits, cut and batched as kick_goal_nn.predict cuts them, so its sigmoid is predict's."""
    import torch
    with torch.no_grad():
        n = len(song.end)
        out = [net(song.slices(torch.arange(s, min(n, s + 2048)))) for s in range(0, n, 2048)]
        return torch.cat(out).numpy() if out else np.zeros(0, np.float32)


def measure(rec, name, x, sr, c):
    from tools.audio_analysis.lab import components
    if name in components.FITTED:
        return np.asarray(components.FITTED[name]['apply'](rec['fitted'][name], x, sr, c), np.float64)
    return np.asarray(components.MEASURES[name](x, sr, c), np.float64)


def reference(rec, x, nets=None, sr=RATE):
    """Every golden quantity for audio x (f64), the stream starting at x[0]."""
    import torch

    from tools.audio_analysis.eval import kick_goal_nn as nnm
    from tools.audio_analysis.eval.detector_eval import fires
    from tools.audio_analysis.lab import components, models, stage
    os.environ['KICK_GOAL_NN_DEVICE'] = 'cpu'
    c = components.audio_candidates(rec['finder'], rec['family'], x, sr)
    k = len(c.cand)
    r = dict(c=c, m={n: measure(rec, n, x, sr, c) for n in measures_of(rec)})
    layers = {}
    if rec['trees']:
        r['p_trees'] = layers['trees'] = rec['trees']['model'].predict_proba(
            np.hstack([r['m'][n] for n in rec['trees']['measures']]))[:, 1]
    if rec['net']:
        n = rec['net']
        r['spec'] = nnm.spectrum(x, sr).astype(np.float32)
        r['frames'] = {ch: components.FRAME_BUILDERS[ch](x, sr).astype(np.float32) for ch in n['channels']}
        song = nnm.Song(r['spec'], c.onset_s, c.emit_s + n['ahead_ms'] / 1000, np.zeros(k, bool), np.zeros(k),
                        extra=tuple(r['frames'][ch] for ch in n['channels']))
        nets = nets or net_modules(rec)
        r['net_logit'] = np.stack([net_logits(net, song) for net in nets], axis=1)
        probs = [nnm.predict(net, song) if k else np.zeros(0) for net in nets]
        # D9: each net's probability is the lab's, sigmoid of the f32 logit in f32, widened.
        for j, p in enumerate(probs):
            if not np.array_equal(torch.sigmoid(torch.from_numpy(np.ascontiguousarray(r['net_logit'][:, j]))).numpy().astype(np.float64), p):
                raise SystemExit(f'net {j}: predict is not the sigmoid of its own logits')
        r['p_net'] = layers['net'] = models.mean_logit_of(probs, CLIP)
    r['p_combined'] = models.combine(rec['combine'], layers)
    r['p_final'] = r['p_combined']
    if rec['stage']:
        st = rec['stage']
        shape = np.hstack([r['m'][s] for s in st['shape']])
        level = r['m'][st['level'][0]][:, st['level'][1]] if st['level'] else None
        r['stage_x'] = stage.inputs(st, (shape, level, c.emit_s), r['p_combined'])
        r['p_stage'] = st['model'].predict_proba(r['stage_x'])[:, 1]
        r['p_final'] = stage.gated(st, r['stage_x'], r['p_stage'], st['cuts'] if st['gate'] else (.5, .5))
    r['fires'] = fires(r['p_final'], c.avail, rec['cutoff'], sr, rec['refractory_s'], c.hop)
    return r


def golden(rec, a16, r):
    """The clip's container entries (section 6 (Parity), golden schema): present exactly when the model has the part."""
    c, i64, f64 = r['c'], np.int64, np.float64
    out = {
        'train_id': kick_container.text(rec['train_id']), 'contract': np.array(CONTRACT, np.int32),
        'rate': np.array(RATE, i64), 'hop': np.array(c.hop, i64), 'audio_i16': a16,
        'cand_hop': c.cand.astype(i64), 'avail_hop': c.avail.astype(i64),
        'onset_s': c.onset_s.astype(f64), 'emit_s': c.emit_s.astype(f64),
        **{f'm.{n}': v.astype(f64) for n, v in r['m'].items()},
    }
    if rec['net']:
        out['frames.spec'] = r['spec']
        out.update({f'frames.{ch}': v for ch, v in r['frames'].items()})
        out['net.logit'] = r['net_logit'].astype(np.float32)
        out['p.net'] = r['p_net'].astype(f64)
    if rec['trees']:
        out['p.trees'] = r['p_trees'].astype(f64)
    out['p.combined'] = np.asarray(r['p_combined'], f64)
    if rec['stage']:
        out['stage.x'] = r['stage_x'].astype(f64)
        out['p.stage'] = r['p_stage'].astype(f64)
    out['p.final'] = np.asarray(r['p_final'], f64)
    out['fires'] = np.asarray(r['fires']).astype(i64)
    return out


# ---------------------------------------------------------------- clips

def clip_specs(rec):
    """{clip: spec} by the record's clip kind. Parts are (fixture, start s, fade-in s) or ('silence', s); the end
    lands in the first candidate-free gap at or after target_s. v1 kinds rebuild the v1 clips exactly."""
    lead = ('silence', LEAD_SILENCE_S)
    windows = rec['stage']['windows_s'] if rec['stage'] else []
    return {
        'kick-v1': dict(dense=dict(parts=[TEARS, APRICOTS], target_s=20.0), quiet_start=dict(parts=QUIET, target_s=12.0)),
        'snare-v1': dict(dense=dict(parts=[lead, TEARS, APRICOTS], target_s=18.0), quiet_start=dict(parts=QUIET, target_s=12.0)),
        'v2': dict(dense=dict(parts=[lead, TEARS, APRICOTS], target_s=max(20.0, 2.5 * max(windows, default=0.0))),
                   quiet_start=dict(parts=QUIET, target_s=12.0)),
        'cover': dict(dense=dict(parts=[lead, TEARS, APRICOTS], target_s=12.0)),
    }[rec['clips']]


def end_margin(rec):
    """Seconds past the last emission a clip must run: the net's look-ahead, one 2 ms frame and the slice jitter
    guard, so no part ever meets the end of the clip (a stream never ends)."""
    return max(.060, (rec['net']['ahead_ms'] / 1000 if rec['net'] else 0.0) + .020)


def quantise(x):
    return np.clip(np.round(x * 32768.0), -32768, 32767).astype(np.int16)


def build_clip(rec, spec):
    """(i16 audio, description). The end sits in a candidate-free gap end_margin past the last emission."""
    from tools.audio_analysis.eval.kick_attack_rejection import read_audio
    from tools.audio_analysis.lab import components
    out, desc = [], []
    for p in spec['parts']:
        if p[0] == 'silence':
            out.append(np.zeros(int(round(p[1] * RATE))))
            desc.append(f'{p[1]:.2f} s digital silence')
            continue
        name, start, fade = p
        x = read_audio(FIXTURES / name / 'mix.wav', RATE)[1][int(round(start * RATE)):].copy()
        if fade:
            n = int(round(fade * RATE))
            x[:n] *= np.linspace(0.0, 1.0, n)
        out.append(x)
        desc.append(f'{name}/mix.wav from {start:.2f} s' + (f' with a {fade:.1f} s linear fade-in' if fade else ''))
    a16 = quantise(np.concatenate(out))
    x = a16 / 32768.0
    # Candidates are causal: those of a prefix are the long clip's with emission inside the prefix.
    c = components.audio_candidates(rec['finder'], rec['family'], x, RATE)
    margin = end_margin(rec)
    for a, b in zip(c.emit_s, np.append(c.emit_s[1:], len(x) / RATE)):
        end_s = a + margin
        if end_s >= spec['target_s'] and end_s < b:
            end = int(np.ceil(end_s * RATE)) + 37  # odd, so the clip never ends on a hop boundary
            if end / RATE < b:
                return a16[:end], ' + '.join(desc) + f', cut at sample {end} ({end / RATE:.4f} s)'
    raise RuntimeError(f'no candidate-free gap after {spec["target_s"]} s')


# ---------------------------------------------------------------- export

def write_checked(path, entries, what):
    kick_container.write(path, entries)
    back = kick_container.read(path)
    bad = [k for k, v in entries.items()
           if k not in back or not (np.array_equal(back[k], v) and back[k].dtype == np.asarray(v).dtype and np.all(np.isfinite(v)))]
    if bad or set(back) != set(entries):
        raise SystemExit(f'{what}: entries not finite or not round-tripping: {bad or sorted(set(back) ^ set(entries))}')
    return back


def export(train_id, set_name, ship=False, out=None):
    t0 = time.time()
    rec = open_release(train_id)
    from tools.audio_analysis.eval import kick_goal_nn as nnm
    from tools.audio_analysis.lab import contract
    root = Path(out) if out else ROOT
    pts = parts()
    model = contract.entries(rec)
    contract.check(model, pts)
    print(f'release {train_id}: {rec["family"]}, finder {rec["finder"]}, measures {measures_of(rec)}, '
          f'nets {len(rec["net"]["state_dicts"]) if rec["net"] else 0}, combine {rec["combine"]}, '
          f'stage {"on" if rec["stage"] else "off"}, cutoff {rec["cutoff"]:.6f}', flush=True)
    d = root / SETS / set_name
    d.mkdir(parents=True, exist_ok=True)
    for old in d.glob('clip_*.mkick'):
        old.unlink()
    nets = net_modules(rec) if rec['net'] else None
    margin = end_margin(rec)
    for name, spec in clip_specs(rec).items():
        a16, desc = build_clip(rec, spec)
        x = a16 / 32768.0
        r = reference(rec, x, nets)
        c, dur = r['c'], len(x) / RATE
        if len(c.emit_s) and c.emit_s[-1] > dur - margin:
            raise SystemExit(f'clip {name}: last emission {c.emit_s[-1]:.4f} s is within {margin} s of the end')
        if spec['parts'][0][0] == 'silence' and rec['net'] and len(c.cand) and (
                c.onset_s[0] - nnm.PRE_S < 0 or c.emit_s[0] + rec['net']['ahead_ms'] / 1000 - nnm.SLICE * nnm.FRAME_S < 0):
            raise SystemExit(f'clip {name}: the first candidate reaches before the clip start')
        g = golden(rec, a16, r)
        write_checked(d / f'clip_{name}.mkick', g, f'clip {name}')
        print(f'clip {name}: {desc}; {dur:.3f} s, {len(c.cand)} candidates, {len(r["fires"])} fires', flush=True)
    path = d / 'model.mkick'
    back = write_checked(path, model, 'model')
    contract.check(back, pts)
    print(f'model: {len(model)} entries -> {path} ({path.stat().st_size} bytes)', flush=True)
    if ship:
        asset = root / ASSETS / f'{rec["family"]}_model.mkick'
        asset.write_bytes(path.read_bytes())
        print(f'shipped -> {asset}', flush=True)
    print(f'export done in {time.time() - t0:.0f} s', flush=True)


# ---------------------------------------------------------------- convert v1

def asset_train_id(family):
    return contract_text(kick_container.read(ROOT / ASSETS / f'{family}_model.mkick')['train_id'])


def contract_text(a):
    return bytes(np.asarray(a, np.uint8)).decode()


def convert_v1(family):
    """The shipped v1 model as a release record: its final.pkl numbers under the v2 parts, the same train_id."""
    path, cfg_name = V1[family]
    raw = json.loads((config.EXPERIMENTS / f'{cfg_name}.json').read_text())
    use(raw, cfg_name)
    from tools.audio_analysis.eval.kick_goal_selfsim import POWER
    with open(path, 'rb') as fh:
        final = pickle.load(fh)
    if final['train_id'] != asset_train_id(family):
        sys.exit(f'{path}: train_id {final["train_id"]} is not the shipped asset\'s {asset_train_id(family)}')
    n = final['net']
    rec = dict(contract=CONTRACT, family=family, train_id=final['train_id'], config=raw, run_id='v1', seed=0,
               fitted={}, cutoff=float(final['cutoff']), refractory_s=float(final['refractory_s']), score={},
               clips=f'{family}-v1')
    if family == 'kick':
        assert (n['bands'], n['past_ms'], n['ahead_ms']) == (64, 150, 40), n
        rec.update(finder='kick', combine='mean_logit', songs=[],
                   net=dict(state_dicts=[n['state_dict']], past_ms=n['past_ms'], ahead_ms=n['ahead_ms'], channels=[]),
                   trees=dict(model=final['trees'], measures=list(V1_KICK_TREES)),
                   fitted={'tmpl3': dict(templates=np.asarray(final['templates'], np.float64))},
                   stage=dict(model=final['stage'], windows_s=[float(final['window_s'])], shape=['prof32', 'low16'],
                              level=('f15', 0), gate=0.0, power=POWER, cuts=None))
    else:
        assert (n['bands'], n['past_ms'], n['ahead_ms'], n['channels']) == (64, 190, 40, 3), n
        rec.update(finder='snare:4.5', combine='net', songs=list(final['songs']), trees=None, stage=None,
                   net=dict(state_dicts=[n['state_dict']], past_ms=n['past_ms'], ahead_ms=n['ahead_ms'], channels=['flat']))
    save(rec)
    return rec


# ---------------------------------------------------------------- train

def run_layers(cfg, meta, zs):
    """[{held song: final-layer probabilities}] per training (plan order) from saved seeds zs[0..N-1]: N = 1 reads the
    run's own layer; N > 1 averages the nets by D17 and takes seed S's trees, then combines."""
    from tools.audio_analysis.lab import models
    final = models.final_layer(cfg)
    out = []
    for n in meta['trainings']:
        layer = {}
        for t, pre in models.save_keys(meta['groups'], n).items():
            def key(name):
                return f'{pre}__{name}__{t}' if pre else f'{name}__{t}'
            if len(zs) == 1:
                layer[t] = zs[0][key(final)]
                continue
            lay = {}
            if 'net' in cfg.models:
                lay['net'] = models.mean_logit_of([z[key('net')] for z in zs], CLIP)
            if 'trees' in cfg.models:
                lay['trees'] = zs[0][key('trees')]
            layer[t] = models.combine(cfg.combine, lay)
        out.append(layer)
    return out


def outer_of(meta, layers):
    """{song: its outer prediction}: from the training that held the song's own group out alone."""
    return {t: p for n, lay in zip(meta['trainings'], layers) if len(n['without']) == 1 for t, p in lay.items()}


def release_cutoffs(st, metas, outer, inputs=None, raw=None):
    """(cutoff, cuts) by D5, from outer predictions only. metas: {exam song: models.song_meta}; outer: the final
    layer; with a stage, inputs and raw are the outer stage inputs and its ungated probabilities."""
    from tools.audio_analysis.lab import models, stage
    exam = {u: outer[u] for u in metas}
    cut_detector = models.cutoff(metas, exam)
    if not st:
        return cut_detector, None
    cut_stage = models.cutoff(metas, {u: raw[u] for u in metas})
    cuts = (cut_detector, cut_stage)
    final = {u: stage.gated(st, inputs[u], raw[u], cuts) for u in metas}
    return models.cutoff(metas, final), (cuts if st['gate'] else None)


def pooled_score(metas, probs, th):
    from tools.audio_analysis.eval.detector_eval import prf, score
    use_ = [u for u in probs if not metas[u][0]['positives_only'] and metas[u][0].get('kind') != 'generated']
    sums = np.sum([score(*metas[u][:2], probs[u], metas[u][2], th, *metas[u][3:5]) for u in use_], axis=0)
    r, p, f1 = prf(*(int(v) for v in sums))
    return dict(cutoff=float(th), precision=p, recall=r, f1=f1)


def net_kwargs(cfg, share, epochs=None):
    """kick_goal_nn.train's settings for the config, as run.train_model passes them."""
    n = cfg.net
    return dict(epochs=epochs or n['epochs'], swa_epochs=0 if epochs else n['swa_epochs'], batch=n['batch'], lr=n['lr'],
                precision=n['precision'], **({'synth_share': share} if share is not None else {}))


def final_record(cfg, train_id, run_id, seed, nets, trees, stage_model, cuts, cutoff, songs, score, clips):
    from tools.audio_analysis.eval.kick_goal_selfsim import POWER
    st = cfg.stage
    return dict(
        contract=CONTRACT, family=cfg.family, train_id=train_id,
        config=dict(cfg.key(), train=list(cfg.train), seeds=list(cfg.seeds)), run_id=run_id, seed=seed,
        finder=cfg.candidates,
        net=cfg.net and dict(state_dicts=[{k: v.detach().cpu().numpy().astype(np.float32) for k, v in net.state_dict().items()}
                                          for net in nets],
                             past_ms=cfg.net['past_ms'], ahead_ms=cfg.net['ahead_ms'], channels=list(cfg.net['channels'])),
        trees=trees and dict(model=trees, measures=list(cfg.models['trees']['measures'])),
        fitted={}, combine=cfg.combine,
        stage=st and dict(model=stage_model, windows_s=[float(w) for w in st['windows_s']], shape=list(st['shape']),
                          level=tuple(st['level']) if st['level'] else None, gate=float(st['gate']), power=POWER,
                          cuts=cuts if st['gate'] else None),
        cutoff=float(cutoff), refractory_s=REFRACTORY_S, songs=sorted(songs), score=score, clips=clips)


REFRACTORY_S = .060  # detector_eval.REFR


def train_provisional(cfg, n_nets, seed):
    """Structurally complete in minutes on the CPU, for building the runtime before the final training (D15):
    PROVISIONAL_SONGS exam songs, one epoch per net, trees and stage max_iter 5, stage on in-sample inputs, cuts and
    cutoff from its own coverage clips (provisional_cuts). Numbers are throwaway."""
    from tools.audio_analysis.eval import kick_goal_nn as nnm
    from tools.audio_analysis.lab import models, run, stage
    os.environ['KICK_GOAL_NN_DEVICE'] = 'cpu'
    t0 = time.time()
    lab, songs, meta = run.setup(cfg)
    picks = [t for t in meta['exam'] if not lab[t]['positives_only']][:PROVISIONAL_SONGS]
    sub = {t: lab[t] for t in picks}
    layers, nets, trees, stage_model = {}, [], None, None
    if cfg.net:
        data = run.build(cfg, sub, songs, nnm.device(), log=lambda m: None)
        nets = [nnm.train([data[t] for t in picks], seed + k, **net_kwargs(cfg, None, epochs=1)) for k in range(n_nets)]
        layers['net'] = {t: models.mean_logit_of([nnm.predict(net, data[t]) for net in nets], CLIP) for t in picks}
    if 'trees' in cfg.models:
        rows = run.tree_rows(cfg, sub, songs)
        trees = models.fit_trees(dict(cfg.models['trees'], max_iter=5), [(*rows[t][1], False) for t in picks], seed)
        layers['trees'] = {t: trees.predict_proba(rows[t][0])[:, 1] for t in picks}
    combined = {t: models.combine(cfg.combine, {m: layers[m][t] for m in layers}) for t in picks}
    if cfg.stage:
        part = {t: stage.song_parts(cfg, songs[t], lab[t]) for t in picks}
        xs = {t: stage.inputs(cfg.stage, part[t], combined[t]) for t in picks}
        stage_model = models.fit_trees(dict(cfg.stage, max_iter=5), [
            (xs[t][part[t][3][0]], part[t][3][1][part[t][3][0]], part[t][3][2][part[t][3][0]], False) for t in picks], seed)
    train_id = f'provisional-{cfg.name}-{datetime.datetime.now().strftime("%Y%m%d-%H%M%S")}'
    rec = final_record(cfg, train_id, 'provisional', seed, nets, trees, stage_model, (.5, .5), .5, picks, {}, 'cover')
    provisional_cuts(rec)
    print(f'{train_id}: {len(picks)} songs, {n_nets} nets, cutoff {rec["cutoff"]:.4f}, '
          f'cuts {rec["stage"]["cuts"] if rec["stage"] else None}, {time.time() - t0:.0f} s', flush=True)
    save(rec)
    return rec


# Provisional cuts and cutoff: quantiles of the record's own scores on its coverage clips, so the goldens exercise
# the gate (two different cuts) and the fire rule (fires on every clip). Throwaway numbers, never a release rule.
Q_DETECTOR, Q_STAGE, Q_CUTOFF = .75, .40, .90


def provisional_cuts(rec):
    """Sets rec's stage cuts (gate on) and cutoff from quantiles of p.combined, the ungated p.stage and the gated
    p.final over the coverage clips, then checks every clip fires and the cuts differ."""
    from tools.audio_analysis.lab import stage
    nets = net_modules(rec) if rec['net'] else None
    runs = [reference(rec, build_clip(rec, spec)[0] / 32768.0, nets) for spec in clip_specs(rec).values()]
    st = rec['stage']
    if st and st['gate']:
        st['cuts'] = (float(np.quantile(np.concatenate([r['p_combined'] for r in runs]), Q_DETECTOR)),
                      float(np.quantile(np.concatenate([r['p_stage'] for r in runs]), Q_STAGE)))
        if st['cuts'][0] == st['cuts'][1]:
            raise SystemExit(f'provisional cuts are equal ({st["cuts"]}): the goldens would not tell them apart')
        final = [stage.gated(st, r['stage_x'], r['p_stage'], st['cuts']) for r in runs]
    else:
        final = [r['p_final'] for r in runs]
    rec['cutoff'] = float(np.quantile(np.concatenate(final), Q_CUTOFF))
    for name, spec in clip_specs(rec).items():
        if not len(reference(rec, build_clip(rec, spec)[0] / 32768.0, nets)['fires']):
            raise SystemExit(f'provisional clip {name} has no fires at cutoff {rec["cutoff"]}')


def train_final(cfg, n_nets, seed):
    """D5: final models on every trained song; cutoffs from the saved run's outer predictions (seeds
    seed..seed+n_nets-1 of this code and labels)."""
    from tools.audio_analysis.eval import kick_goal_nn as nnm
    from tools.audio_analysis.lab import components, models, run, stage
    t0 = time.time()
    gid = git_id()
    if gid.endswith('-dirty'):
        sys.exit('tools/audio_analysis has uncommitted changes: a release train_id must name the code that made it')
    lab, songs, meta = run.setup(cfg)
    # The run id hashes the lab modules loaded in its process; this one also loads the release modules.
    meta['run_id'] = run.run_id_of(cfg.name)
    d = run.RUNS / meta['run_id']
    paths = [d / f's{s}.npz' for s in range(seed, seed + n_nets)]
    missing = [p.name for p in paths if not p.exists()]
    if missing:
        sys.exit(f'run {meta["run_id"]} lacks seeds {missing}: lab/run.py {cfg.name} --seeds '
                 + ','.join(str(s) for s in range(seed, seed + n_nets)))
    zs = [dict(np.load(p)) for p in paths]
    layers = run_layers(cfg, meta, zs)
    outer = outer_of(meta, layers)
    exam = meta['exam']
    lost = [t for t in exam if t not in outer]
    if lost:
        sys.exit(f'exam songs without an outer prediction: {lost}')
    metas = {t: models.song_meta(lab[t], components.candidates(songs[t], cfg.candidates, cfg.family, build=False))
             for t in exam}
    st = cfg.stage
    grps, trainings = meta['groups'], meta['trainings']
    if st:
        part = {t: stage.song_parts(cfg, songs[t], lab[t], build=False) for t in lab}
        if n_nets == 1:
            if any(f'stageraw__{t}' not in zs[0] for t in exam):
                sys.exit(f'run {meta["run_id"]} predates the stageraw save; train its seeds again')
            raw = {t: zs[0][f'stageraw__{t}'] for t in lab if f'stageraw__{t}' in zs[0]}
        else:
            # The averaged layer is a new detector: its outer stages are judged again on it, by the lab's own stage.
            got = stage.train(cfg, grps, trainings, layers, part, seed, generated=meta['generated'])
            raw = {t: got[f'stageraw__{t}'] for t in lab}
        inputs = {t: stage.inputs(st, part[t], outer[t]) for t in outer}
    else:
        inputs = raw = None
    cutoff, cuts = release_cutoffs(st, metas, outer, inputs, raw)
    score = {models.final_layer(cfg): pooled_score(metas, {t: outer[t] for t in exam}, models.cutoff(metas, outer))}
    if st:
        cut_pair = cuts or (.5, .5)
        score['release'] = pooled_score(metas, {t: stage.gated(st, inputs[t], raw[t], cut_pair) for t in exam}, cutoff)
    print(f'{cfg.name}: cutoff {cutoff:.4f} cuts {cuts} | {score}', flush=True)

    _, (tr,), _ = models.plan(cfg, lab, songs, final=True)
    share = cfg.sampling['generated'] if cfg.generated_as_clips() else None
    trained = tr['trained'] + tr['clips']
    nets, trees, stage_model = [], None, None
    if 'trees' in cfg.models:
        rows = run.tree_rows(cfg, lab, songs)
        trees = models.fit_trees(cfg.models['trees'], [(*rows[u][1], u in tr['clips']) for u in trained], seed, share)
    if cfg.net:
        sys.path.insert(0, str(ROOT / 'scripts'))
        import gpu_queue
        data = run.build(cfg, lab, songs, nnm.device(), log=lambda m: None)
        with gpu_queue.hold('detector-release'):
            nets = [nnm.train([data[u] for u in tr['trained']], seed + k, [data[c] for c in tr['clips']], **net_kwargs(cfg, share))
                    for k in range(n_nets)]
    if st:
        learn = {u for n in trainings for u in n['trained'] + n['clips']}
        if st['learn'] == 'project':
            learn -= set(meta['generated'])
        clips = {u for n in trainings for u in n['clips']}
        rows = []
        for u in sorted(learn):
            mask, y, hard = part[u][3]
            rows.append((inputs[u][mask], y[mask], hard[mask], u in clips))
        stage_model = models.fit_trees(st, rows, seed, share)
    train_id = f'{cfg.family}-{datetime.date.today().isoformat()}-{gid}'
    rec = final_record(cfg, train_id, meta['run_id'], seed, nets, trees, stage_model, cuts, cutoff, trained, score, 'v2')
    print(f'{train_id}: final models in {time.time() - t0:.0f} s', flush=True)
    save(rec)
    return rec


def train(name, seed, n_nets, provisional):
    cfg = config.load(name)
    if n_nets < 1:
        sys.exit('--nets is at least 1')
    refuse_undeclared(cfg)
    use(cfg.key(), cfg.name)
    if cfg.folds == 0 and cfg.stage:
        sys.exit(f'{cfg.name}: a stage needs song groups')
    return train_provisional(cfg, n_nets, seed) if provisional else train_final(cfg, n_nets, seed)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    t = sub.add_parser('train')
    t.add_argument('config')
    t.add_argument('--seed', type=int, default=0)
    t.add_argument('--nets', type=int, default=1)
    t.add_argument('--provisional', action='store_true')
    c = sub.add_parser('convert-v1')
    c.add_argument('family', choices=sorted(V1))
    e = sub.add_parser('export')
    e.add_argument('train_id')
    e.add_argument('--set', required=True, dest='set_name')
    e.add_argument('--ship', action='store_true')
    e.add_argument('--out')
    sub.add_parser('parts')
    a = ap.parse_args()
    if a.cmd == 'train':
        train(a.config, a.seed, a.nets, a.provisional)
    elif a.cmd == 'convert-v1':
        convert_v1(a.family)
    elif a.cmd == 'export':
        export(a.train_id, a.set_name, a.ship, a.out)
    else:
        recipe.apply()
        bad = check_parts()
        print('\n'.join(bad) or f'{PARTS.relative_to(ROOT)}: every part exists in the lab with its width')
        sys.exit(1 if bad else 0)


if __name__ == '__main__':
    main()
