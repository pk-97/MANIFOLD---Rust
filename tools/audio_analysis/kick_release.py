#!/usr/bin/env python3
"""Kick model release: fit the final model, then export the model file and the Rust parity goldens.

Usage:
  kick_release.py train                  the final model: grouped run (held-out score), then fits on every song
  kick_release.py train --provisional    quick, structurally complete final.pkl (numbers are throwaway)
  kick_release.py export --hooks DIR --out DIR

The recipe is the winning fast-loop config (docs/KICK_REALTIME_DESIGN.md section 1): f69 trees, one CNN whose
slice ends 40 ms after emission, mean-logit blend, the 8 s song-relative stage, cutoff, 60 ms refractory. Its
env flags are forced here, so the research modules import with the recipe's song set and net input.

export writes <out>/assets/kick_model.mkick (recipe_version, train_id and every kick_export/<piece>.py hook's
model_entries) and <out>/tests/fixtures/kick/clip_<name>.mkick: the Python reference run on each clip alone,
the stream starting at the clip's first sample. Before writing, it proves the reference reproduces the trained
recipe: on whole training songs, the clip pipeline must give the cached candidates and f69 rows.
"""
from __future__ import annotations

import os
import sys

RECIPE_ENV = dict(KICK_GOAL_MORE='1', KICK_GOAL_TRIGGER='1', KICK_GOAL_WIP='1', KICK_GOAL_TRUTH='v3', KICK_GOAL_PROJECT='1',
                  KICK_GOAL_RECALL='1', KICK_GOAL_MELODIC='1', KICK_GOAL_NN_AHEAD_MS='40')
# Net input settings other than the recipe's would silently change the net's shape.
OFF_RECIPE = ('KICK_GOAL_NN_BANDS', 'KICK_GOAL_NN_PAST_MS', 'KICK_GOAL_NN_PERC', 'KICK_GOAL_NN_STEREO', 'KICK_GOAL_NN_SYNTH',
              'KICK_GOAL_NN_FLAT', 'KICK_GOAL_WIP2')
for _k, _v in RECIPE_ENV.items():
    if os.environ.get(_k, _v) != _v:
        sys.exit(f'{_k}={os.environ[_k]} is not the release recipe ({_v})')
    os.environ[_k] = _v
for _k in OFF_RECIPE:
    if os.environ.get(_k):
        sys.exit(f'{_k} is set; the release recipe uses the net defaults')
for _k in ('OMP_NUM_THREADS', 'OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'MKL_NUM_THREADS'):
    os.environ.setdefault(_k, '1')

import argparse  # noqa: E402
import datetime  # noqa: E402
import importlib.util  # noqa: E402
import pickle  # noqa: E402
import time  # noqa: E402
from pathlib import Path  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402
from scipy.special import expit, logit  # noqa: E402

from tools.audio_analysis import kick_container  # noqa: E402
from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, REFRACTORY, TRACKS, TRUTH, fires  # noqa: E402
from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes, lowbank_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_profile import rise_profile  # noqa: E402
from tools.audio_analysis.eval.kick_goal_selfsim import self_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_tail_features import tail_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_templates import band_spec, patches, template_features  # noqa: E402
from tools.audio_analysis.eval import kick_goal_nn as nnm  # noqa: E402

FINAL = GOAL / 'final' / 'final.pkl'
RECIPE_VERSION = 1
SR = 48000
WINDOW_S = 8.0
# The fixture audio is not in git: the research code's main-checkout path.
FIXTURES = Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio')
# A golden candidate needs audio up to its emission + the net's 40 ms look-ahead + one 2 ms frame + the slice jitter
# guard; the clip end is placed in a candidate-free gap at least this far past the last emission, so no piece ever
# meets the end of the clip (Python clips or zeroes rows there; a stream never ends).
END_MARGIN_S = .060
HOOK_PIECES = {'base': ('base.',), 'extra': ('extra.',), 'net': ('net.',), 'stage': ('stage.', 'trees.')}
# Golden clips. Parts are (fixture, start s, fade-in s) or ('silence', seconds); the end lands in the first
# candidate-free gap at or after target_s.
CLIPS = {
    # Dense, ~20 s, so the 8 s stage memory is full for over half the clip; the hard cut between songs stays in.
    'dense': dict(parts=[('tears_140bpm', 0.0, 0.0), ('apricots_128bpm', 0.0, 0.0)], target_s=20.0),
    # Quiet start, ~12 s: digital silence, then a 44.1 kHz fixture (resampled) fading in over 2 s.
    'quiet_start': dict(parts=[('silence', .75), ('bad_guy_128bpm', 0.0, 2.0)], target_s=12.0),
}


def lg(p):
    return logit(np.clip(p, 1e-6, 1 - 1e-6))


def load_final(path=FINAL):
    """The final-model dict (kick contract): train_id, trees, net, stage, templates, cutoff, refractory_s, window_s."""
    with open(path, 'rb') as fh:
        return pickle.load(fh)


def net_module(final):
    """The torch Net with the final weights, on the CPU, in eval mode."""
    import torch
    n = final['net']
    assert (n['bands'], n['past_ms'], n['ahead_ms']) == (nnm.BANDS, nnm.PAST_MS, nnm.AHEAD_MS), 'net input settings differ'
    net = nnm.Net()
    net.load_state_dict({k: torch.from_numpy(np.asarray(v, np.float32)) for k, v in n['state_dict'].items()})
    return net.eval()


# ---------------------------------------------------------------- train

def setup_goal():
    from tools.audio_analysis.eval.kick_goal_eval import Goal, add_whole_song_truth
    from tools.audio_analysis.eval.kick_goal_featsets import build
    from tools.audio_analysis.eval.kick_goal_melodic import add_melodic_negatives
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    add_melodic_negatives(g)
    build(g, 'f69')
    return g


def final_dict(train_id, trees, net, stage, templates, cutoff):
    return dict(
        train_id=train_id,
        trees=trees,
        net=dict(state_dict={k: v.detach().cpu().numpy().astype(np.float32) for k, v in net.state_dict().items()},
                 bands=nnm.BANDS, past_ms=nnm.PAST_MS, ahead_ms=nnm.AHEAD_MS),
        stage=stage,
        templates=np.asarray(templates, np.float64),
        cutoff=float(cutoff),
        refractory_s=REFRACTORY,
        window_s=WINDOW_S,
    )


def write_final(final, t0):
    FINAL.parent.mkdir(parents=True, exist_ok=True)
    tmp = FINAL.with_name(f'{FINAL.name}.{os.getpid()}.tmp')
    with open(tmp, 'wb') as fh:
        pickle.dump(final, fh)
    os.replace(tmp, FINAL)
    print(f'wrote {FINAL}\ntrain_id {final["train_id"]} | trees n_iter_ {final["trees"].n_iter_} | '
          f'stage n_iter_ {final["stage"].n_iter_} | cutoff {final["cutoff"]:.6f} | templates {final["templates"].shape} | '
          f'total {time.time() - t0:.0f} s', flush=True)


def train_provisional():
    """Structurally complete and quick: in-sample stage inputs, a 2-epoch net, cutoff 0.5. Numbers are throwaway."""
    from tools.audio_analysis.eval.kick_goal_featsets import lowbank_cache, profile_cache
    from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt
    from tools.audio_analysis.eval.run_kick_goal_selfsim import fit2
    t0 = time.time()
    g = setup_goal()
    print(f'songs {len(ALL)}, records ready at {time.time() - t0:.0f} s', flush=True)
    fit, pred = gbt(g, True, 'f69')
    trees = fit(list(ALL))
    print(f'trees fitted at {time.time() - t0:.0f} s', flush=True)
    nnm.EPOCHS = 2
    dev = nnm.device()
    data = {t: nnm.Song.real(g, t).to(dev) for t in ALL}
    net = nnm.train([data[t] for t in ALL], 7)
    print(f'net fitted ({nnm.EPOCHS} epochs, {dev}) at {time.time() - t0:.0f} s', flush=True)
    blend = {t: expit((lg(pred(trees, t)) + lg(nnm.predict(net, data[t]))) / 2) for t in ALL}
    shape = {t: np.hstack([profile_cache(g, t), lowbank_cache(g, t)]) for t in ALL}
    inputs = {t: self_features(blend[t], shape[t], g.records[t]['features'][:, 0], g.records[t]['emit_s'], WINDOW_S)
              for t in ALL}
    stage = fit2(g, inputs, list(ALL), slice(None))
    print(f'stage fitted at {time.time() - t0:.0f} s', flush=True)
    train_id = f'provisional-{datetime.date.today().isoformat()}-{datetime.datetime.now().strftime("%H%M%S")}'
    write_final(final_dict(train_id, trees, net, stage, np.load(GOAL / 'templates_from_dev.npy'), 0.5), t0)


def all_song_templates(g):
    """K-means templates over every isolated kick source the recipe's template features were built from
    (run_kick_goal_templates.template_sets): the four new songs' kick stems at their fresh attacks, the dev songs'
    kick stems at their fresh attacks, and the original five's drum stems at their truth. The research fitted
    two cross sets from these; the release fits one set on their union, fit_templates seed 0. Refitting the two
    cross sets must reproduce the cached ones, which proves the patch sources are the recipe's."""
    import types
    from tools.audio_analysis.eval import run_kick_goal_templates as rt
    from tools.audio_analysis.eval.kick_goal_eval import NEW_SONGS
    from tools.audio_analysis.eval.kick_goal_templates import fit_templates
    lists = []
    real_fit = rt.fit_templates
    rt.fit_templates = lambda patch_list, seed=0: lists.append(patch_list)
    try:
        rt.template_sets(types.SimpleNamespace(records={t: g.records[t] for t in tuple(TRACKS) + tuple(NEW_SONGS)}))
    finally:
        rt.fit_templates = real_fit
    new_p, dev_p = lists
    for name, p in (('templates_from_new', new_p), ('templates_from_dev', dev_p)):
        d = float(np.max(np.abs(fit_templates(p) - np.load(GOAL / f'{name}.npy'))))
        print(f'templates: refit {name} vs its cache, max diff {d:.3g}', flush=True)
        if d > 1e-9:
            raise SystemExit(f'template sources differ from the recipe ({name})')
    n = sum(len(x) for x in new_p + dev_p)
    print(f'templates: {len(new_p) + len(dev_p)} sources, {n} patches', flush=True)
    return fit_templates(new_p + dev_p)


def git_id():
    import subprocess
    sha = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True,
                         check=True).stdout.strip()
    dirty = subprocess.run(['git', '-C', str(ROOT), 'status', '--porcelain', '--', 'tools/audio_analysis'],
                           capture_output=True, text=True, check=True).stdout.strip()
    return sha + ('-dirty' if dirty else '')


def train_final():
    """The tested recipe with one net. A grouped run (run_kick_goal_fast's machinery, unchanged) gives every song
    out-of-group tree, net and blend probabilities and the held-out score; the final stage is fitted on stage
    inputs built from those blends and the cutoff is chosen on the out-of-group staged predictions; trees,
    templates and the net are then fitted on every song."""
    from concurrent.futures import ProcessPoolExecutor
    from multiprocessing import get_context
    from tools.audio_analysis.eval import run_kick_goal_fast as fast
    from tools.audio_analysis.eval.kick_goal_eval import RECALL_SONGS, SUFFIX, choose
    from tools.audio_analysis.eval.kick_goal_featsets import lowbank_cache, profile_cache
    from tools.audio_analysis.eval.run_kick_goal_data import ALL
    from tools.audio_analysis.eval.run_kick_goal_selfsim import fit2
    assert (fast.NETS, fast.SEED, fast.SYNTH_ON, fast.MELODIC_ON, fast.WINDOWS, fast.FEATS) == (1, 0, False, True, [WINDOW_S], 'f69')
    assert nnm.EPOCHS == 12 and nnm.AHEAD_MS == 40
    t0 = time.time()
    train_id = f'kick-{datetime.date.today().isoformat()}-{git_id()}'
    folds = fast.groups()
    fast.setup()
    g = fast.STATE['g']
    group_of = {t: k for k, f in enumerate(folds) for t in f}
    print(f'{train_id}: {len(ALL)} songs in {len(folds)} groups, records ready at {time.time() - t0:.0f} s', flush=True)

    # Grouped run: run_kick_goal_fast.main's steps, kept in step with it.
    tasks = [((f,), f) for f in folds]
    keys = [(k, None) for k in range(len(folds))]
    for i in range(len(folds)):
        for j in range(i + 1, len(folds)):
            tasks.append(((folds[i], folds[j]), folds[i] + folds[j]))
            keys.append((i, j))
    tree_out, tree_in = {}, {}
    tree_path = fast.OUT / f'fast_treecache{SUFFIX}{"_mel" if fast.MELODIC_ON else ""}.npz'
    with ProcessPoolExecutor(fast.WORKERS, mp_context=get_context('spawn'), initializer=fast.setup) as ex:
        cached = tree_path.exists()
        futures = [] if cached else [ex.submit(fast.tree_task, (tuple(u for f in out for u in f), scored)) for out, scored in tasks]
        net_p = fast.nets(g, folds)
        print(f'grouped nets done at {time.time() - t0:.0f} s', flush=True)
        if cached:
            with np.load(tree_path) as z:
                for key in z.files:
                    i, j, u = key.split('|')
                    (tree_out.setdefault(int(i), {}) if j == '' else tree_in.setdefault((int(i), int(j)), {}))[u] = z[key]
        else:
            for (i, j), fut in zip(keys, futures):
                p = fut.result()
                if j is None:
                    tree_out[i] = p
                else:
                    tree_in[(i, j)] = {u: p[u] for u in folds[j]}
                    tree_in[(j, i)] = {u: p[u] for u in folds[i]}
            np.savez(tree_path, **{f'{i}||{u}': v for i, d in tree_out.items() for u, v in d.items()},
                     **{f'{i}|{j}|{u}': v for (i, j), d in tree_in.items() for u, v in d.items()})
        print(f'grouped trees {"loaded from " + tree_path.name if cached else "done"} at {time.time() - t0:.0f} s', flush=True)
        net_of = {t: net_p[k][t] for k, f in enumerate(folds) for t in f}

        def blend(k, j, t):
            return expit((fast.lg(tree_in[(k, j)][t] if j is not None else tree_out[k][t]) + fast.lg(net_of[t])) / 2)
        raw = {t: blend(k, None, t) for k, f in enumerate(folds) for t in f}
        raw_cut = {}
        for k, f in enumerate(folds):
            th, _ = choose(g, {t: blend(k, j, t) for j, h in enumerate(folds) if j != k for t in h if t not in RECALL_SONGS})
            raw_cut.update({t: th for t in f})
        fast.report(g, 'blend', raw, raw_cut)
        stage_tasks = [(k, folds, {t: blend(k, j, t) for j, h in enumerate(folds) if j != k for t in h},
                        {t: blend(k, None, t) for t in f}) for k, f in enumerate(folds)]
        staged, cuts = {}, {}
        for k, (p, th) in enumerate(ex.map(fast.stage_task, stage_tasks)):
            staged.update(p)
            cuts.update({t: th for t in folds[k]})
        fast.report(g, 'blend+stage', staged, cuts)
    print(f'grouped run done at {time.time() - t0:.0f} s (held-out score above)', flush=True)

    # Final stage: every song's stage inputs from its out-of-group blend; cutoff on the out-of-group staged scores.
    shape = {t: np.hstack([profile_cache(g, t), lowbank_cache(g, t)]) for t in ALL}
    inputs = {t: fast.stage_matrix(g, shape, t, blend(group_of[t], None, t)) for t in ALL}
    stage = fit2(g, inputs, list(ALL), slice(None))
    cutoff, f1 = choose(g, {t: staged[t] for t in ALL if t not in RECALL_SONGS})
    print(f'final stage fitted; cutoff {cutoff:.6f} (pooled F1 {f1:.4f} on out-of-group staged scores, '
          f'recall-only songs left out) at {time.time() - t0:.0f} s', flush=True)

    trees = fast.STATE['fit'](list(ALL))
    print(f'final trees fitted at {time.time() - t0:.0f} s', flush=True)
    templates = all_song_templates(g)
    print(f'final templates {templates.shape} at {time.time() - t0:.0f} s', flush=True)
    dev = nnm.device()
    data = {t: nnm.Song.real(g, t).to(dev) for t in ALL}
    net = nnm.train([data[t] for t in ALL], 0)
    print(f'final net fitted ({nnm.EPOCHS} epochs, seed 0, {dev}) at {time.time() - t0:.0f} s', flush=True)
    write_final(final_dict(train_id, trees, net, stage, templates, cutoff), t0)


def train(provisional):
    train_provisional() if provisional else train_final()


# ---------------------------------------------------------------- reference pipeline

def features(x, templates, sr=SR):
    """Candidates and the f69 parts for audio x (f64, sr), the stream starting at x[0]."""
    cand, avail, f15, hop = fusion_features(x, sr)
    onset_s = (cand + 1) * hop / sr
    emit_s = (avail + 1) * hop / sr
    spec = band_spec(x, sr, hop)
    out = dict(hop=hop, cand_hop=cand, avail_hop=avail, onset_s=onset_s, emit_s=emit_s, f15=f15,
               tail3=tail_features(x, sr, onset_s, emit_s),
               tmpl3=template_features(patches(spec, cand, avail), templates),
               prof32=rise_profile(spec, cand, avail),
               low16=lowbank_features(band_envelopes(x, sr), onset_s, emit_s))
    out['f69'] = np.hstack([out[k] for k in ('f15', 'tail3', 'tmpl3', 'prof32', 'low16')])
    return out


def reference(x, final, net, sr=SR):
    """Every golden array for audio x."""
    os.environ['KICK_GOAL_NN_DEVICE'] = 'cpu'
    r = features(x, final['templates'], sr)
    k = len(r['cand_hop'])
    r['tree_p'] = final['trees'].predict_proba(r['f69'])[:, 1]
    # f32 throughout, never the float16 training caches.
    r['net_spec'] = nnm.spectrum(x, sr).astype(np.float32)
    song = nnm.Song(r['net_spec'], r['onset_s'], r['emit_s'] + nnm.AHEAD_MS / 1000, np.zeros(k, bool), np.zeros(k))
    r['net_p'] = nnm.predict(net, song)
    r['blend_p'] = expit((lg(r['tree_p']) + lg(r['net_p'])) / 2)
    r['stage_x'] = self_features(r['blend_p'], np.hstack([r['prof32'], r['low16']]), r['f15'][:, 0], r['emit_s'],
                                 final['window_s'])
    r['stage_p'] = final['stage'].predict_proba(r['stage_x'])[:, 1]
    r['fires'] = fires(r['stage_p'], r['avail_hop'], sr, r['hop'], final['cutoff'])
    return r


# ---------------------------------------------------------------- clips

def fixture(name):
    return read_audio(FIXTURES / name / 'mix.wav', SR)[1]


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
        x = fixture(name)[int(round(start * SR)):].copy()
        if fade:
            n = int(round(fade * SR))
            x[:n] *= np.linspace(0.0, 1.0, n)
        parts.append(x)
        desc.append(f'{name}/mix.wav from {start:.2f} s' + (f' with a {fade:.1f} s linear fade-in' if fade else ''))
    a16 = quantise(np.concatenate(parts))
    x = a16 / 32768.0
    # Candidates are causal: those of a prefix are the long clip's with emission inside the prefix.
    cand, avail, _, hop = fusion_features(x, SR)
    emit = (avail + 1) * hop / SR
    target = spec['target_s']
    for a, b in zip(emit, np.append(emit[1:], len(x) / SR)):
        end_s = a + END_MARGIN_S
        if end_s >= target and end_s < b:
            # Odd sample count past the margin, so the clip never ends on a hop boundary.
            end = int(np.ceil(end_s * SR)) + 37
            if end / SR < b:
                return a16[:end], ' + '.join(desc) + f', cut at sample {end} ({end / SR:.4f} s)'
    raise RuntimeError(f'no candidate-free gap after {target} s')


# ---------------------------------------------------------------- self-check

def self_check():
    """On whole training songs, the clip pipeline must reproduce the cached candidates and f69 rows. Templates are
    the cross-fitted set each song's tmpl cache was made with (run_kick_goal_templates, run_kick_goal_more_prep)."""
    from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio
    g = setup_goal()
    worst = 0.0
    for t in ('apricots_128bpm', 'gerrit'):
        t0 = time.time()
        rec = g.records[t]
        assert rec['sample_rate'] == SR, f'{t}: self-check needs a 48 kHz song'
        templates = np.load(GOAL / ('templates_from_new.npy' if t in TRACKS else 'templates_from_dev.npy'))
        x = mix_audio(g, t)
        r = features(x, templates)
        if not (np.array_equal(r['cand_hop'], rec['candidates']) and np.array_equal(r['avail_hop'], rec['available'])):
            raise SystemExit(f'self-check {t}: candidates differ ({len(r["cand_hop"])} vs {len(rec["candidates"])})')
        cached = dict(f15=rec['features'], tail3=np.load(GOAL / f'tail_{t}.npy'), tmpl3=np.load(GOAL / f'tmpl_{t}.npy'),
                      prof32=np.load(GOAL / f'prof_{t}.npy'), low16=np.load(GOAL / f'low_{t}.npy'), f69=rec['f69'])
        # The trained tail caches predate the causal past-4 s max (kick_goal_tail_features.py): pre_rel_db
        # (tail column 1, f69 column 16) differs where that window reached before the song's start.
        early = np.round(rec['onset_s'] / .001).astype(int) - 5 < 1999
        for k, col in (('tail3', 1), ('f69', 16)):
            cached[k] = cached[k].copy()
            cached[k][early, col] = r[k][early, col]
        diffs = {k: float(np.max(np.abs(r[k] - v))) for k, v in cached.items()}
        spec = nnm.spectrum(x, SR)
        cache16 = np.load(GOAL / f'nnspec_{t}.npy').astype(np.float32)
        print(f'self-check {t} ({len(x) / SR:.1f} s, {len(r["cand_hop"])} candidates, identical; {time.time() - t0:.0f} s): '
              + ', '.join(f'{k} {v:.3g}' for k, v in diffs.items())
              + f'; net spectrum vs its float16 cache {float(np.max(np.abs(spec - cache16))):.3g} dB (float16 rounding)',
              flush=True)
        worst = max(worst, diffs['f69'])
    if worst > 1e-9:
        raise SystemExit(f'self-check failed: f69 differs from the trained caches by {worst:.3g}')


# ---------------------------------------------------------------- export

def hook_entries(hooks_dir, final):
    out, owner = {}, {}
    for piece, prefixes in HOOK_PIECES.items():
        path = Path(hooks_dir) / 'kick_export' / f'{piece}.py'
        if not path.exists():
            print(f'hook {piece}: {path} not present, skipped', flush=True)
            continue
        spec = importlib.util.spec_from_file_location(f'kick_export_{piece}', path)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        entries = mod.model_entries(final)
        for name, a in entries.items():
            a = np.asarray(a)
            if not name.startswith(prefixes):
                raise SystemExit(f'hook {piece}: entry {name!r} must start with one of {prefixes}')
            if a.dtype not in kick_container.CODES:
                raise SystemExit(f'hook {piece}: entry {name!r} has dtype {a.dtype}, not a container dtype')
            if name in out:
                raise SystemExit(f'entry {name!r} comes from both {owner[name]} and {piece}')
            out[name], owner[name] = a, piece
        print(f'hook {piece}: {len(entries)} entries', flush=True)
    return out


def export(hooks_dir, out_dir):
    t0 = time.time()
    final = load_final()
    print(f'final model {final["train_id"]}', flush=True)
    self_check()
    print(f'self-check passed at {time.time() - t0:.0f} s', flush=True)
    net = net_module(final)
    gold_dir = Path(out_dir) / 'tests' / 'fixtures' / 'kick'
    gold_dir.mkdir(parents=True, exist_ok=True)
    for name, spec in CLIPS.items():
        a16, desc = build_clip(spec)
        x = a16 / 32768.0
        r = reference(x, final, net)
        dur = len(x) / SR
        if len(r['emit_s']) and r['emit_s'][-1] > dur - END_MARGIN_S:
            raise SystemExit(f'clip {name}: last emission {r["emit_s"][-1]:.4f} s is within {END_MARGIN_S} s of the end')
        i64 = np.int64
        entries = {
            'train_id': kick_container.text(final['train_id']),
            'sample_rate': np.array(SR, i64), 'hop': np.array(r['hop'], i64),
            'audio_i16': a16,
            'cand_hop': r['cand_hop'].astype(i64), 'avail_hop': r['avail_hop'].astype(i64),
            'onset_s': r['onset_s'].astype(np.float64), 'emit_s': r['emit_s'].astype(np.float64),
            **{k: r[k].astype(np.float64) for k in ('f15', 'tail3', 'tmpl3', 'prof32', 'low16', 'f69')},
            'net_spec': r['net_spec'].astype(np.float32),
            **{k: r[k].astype(np.float64) for k in ('net_p', 'tree_p', 'blend_p', 'stage_x', 'stage_p')},
            'fires': r['fires'].astype(i64),
        }
        path = gold_dir / f'clip_{name}.mkick'
        kick_container.write(path, entries)
        back = kick_container.read(path)
        bad = [k for k, v in entries.items() if not (np.array_equal(back[k], v) and np.all(np.isfinite(v)))]
        if bad:
            raise SystemExit(f'clip {name}: entries not finite or not round-tripping: {bad}')
        print(f'clip {name}: {desc}; {dur:.3f} s, {len(r["cand_hop"])} candidates, {len(r["fires"])} fires -> {path}',
              flush=True)
        print('   ' + ', '.join(f'{k} {v.dtype}{list(v.shape)}' for k, v in entries.items()), flush=True)
    model = {'recipe_version': np.array(RECIPE_VERSION, np.int32), 'train_id': kick_container.text(final['train_id']),
             **hook_entries(hooks_dir, final)}
    path = Path(out_dir) / 'assets' / 'kick_model.mkick'
    path.parent.mkdir(parents=True, exist_ok=True)
    kick_container.write(path, model)
    print(f'model: {len(model)} entries -> {path} ({path.stat().st_size} bytes); total {time.time() - t0:.0f} s', flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    t = sub.add_parser('train')
    t.add_argument('--provisional', action='store_true')
    e = sub.add_parser('export')
    e.add_argument('--hooks', required=True)
    e.add_argument('--out', required=True)
    a = ap.parse_args()
    if a.cmd == 'train':
        train(a.provisional)
    else:
        export(a.hooks, a.out)


if __name__ == '__main__':
    main()
