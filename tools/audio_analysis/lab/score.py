#!/usr/bin/env python3
"""Scores a run from its saves (docs/DETECTOR_LAB_DESIGN.md D7, D8): saved held-out probabilities, cached candidates
and the labels; no audio, no GPU. The rules are detector_eval.held_out's: each song at a cutoff chosen on other
project songs, never on generated or recall-only songs; with song groups, on the pair models' predictions, which never
saw the scored group. Writes report_s<seed>.json per seed (report_<layer>_s<seed>.json for a named layer): exam songs
under results.net.per_song (detector_compare's layout), generated songs in their own section.

Usage: score.py RUN_ID [--layer net|trees|combined] [--exam-of OLD_RUN_ID]   (default: the run's final layer;
         --exam-of also prints pooled recall, precision and F1 on the songs of OLD_RUN_ID's exam, beside the full exam)
       score.py --legacy NPZ CONFIG   (a save of a retired runner, scored by these rules under CONFIG)"""
import json
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

from tools.audio_analysis.lab.run import RUNS, labels  # noqa: E402


COINCIDENT_S = .020                     # a partner label this close is the same moment
PARTNER = {'snare': 'kick', 'kick': 'snare'}


def coincident_mask(pos, partner_pos, window=COINCIDENT_S):
    """Which of pos sit within window of a partner label (same song, same clock)."""
    from tools.audio_analysis.eval.detector_eval import EDGE, nearest
    pos = np.asarray(pos, float)
    if not len(pos):
        return np.zeros(0, bool)
    return nearest(pos, partner_pos) <= window + EDGE


def caught_mask(s, fired):
    """Which of a song's positives the fires catch, by the rule judged() counts matches with."""
    from tools.audio_analysis.eval.detector_eval import nearest, tol, within
    pos = np.asarray(s['positives'], float)
    if not len(pos) or not len(fired):
        return np.zeros(len(pos), bool)
    if 'window_s' in s:
        return within(pos, fired, *s['window_s'])
    return nearest(pos, fired) <= tol(s)


def coincident_result(s, partner_pos, stamp, p, avail, th, sr, hop):
    """One song's recall on the labels that fall within COINCIDENT_S of a partner-family label: overall and
    coincident (labels, matched), at cutoff th. Same fires and same match rule as the song's own score."""
    from tools.audio_analysis.eval.detector_eval import fires
    f = stamp[fires(p, avail, th, sr, hop=hop)]
    caught = caught_mask(s, f)
    co = coincident_mask(s['positives'], partner_pos)
    return dict(labels=int(len(caught)), matched=int(caught.sum()),
                coincident_labels=int(co.sum()), coincident_matched=int((caught & co).sum()))


def coincident_pooled(per_song, songs):
    """Summed coincident_result counts over songs, with the recalls (None where nothing to divide)."""
    out = {k: sum(per_song[t][k] for t in songs) for k in ('labels', 'matched', 'coincident_labels', 'coincident_matched')}
    out['songs'] = len(songs)
    out['recall'] = out['matched'] / out['labels'] if out['labels'] else None
    out['coincident_recall'] = (out['coincident_matched'] / out['coincident_labels']
                                if out['coincident_labels'] else None)
    return out


def partner_labels(family):
    """The partner family's labels (snare <-> kick), or None when the family has none or its files are missing."""
    if family not in PARTNER:
        return None
    try:
        return labels(PARTNER[family])[0]
    except FileNotFoundError:
        return None


def score(cfg, lab, songs, held, pool=None, partner=None, coincident=None):
    """{song: song_result} for every song in held, each at its held-out cutoff (chosen on pool[song] when given).
    A family with a tight hit window (labels' tol_s or window_s) is judged at the candidate, and its fixed wait to
    emission counts as lateness. With partner (the other family's labels) and a coincident dict, fills
    coincident[song] = coincident_result for each held song the partner labels; nothing else changes."""
    from tools.audio_analysis.eval.detector_eval import held_out
    from tools.audio_analysis.lab import components
    from tools.audio_analysis.lab.models import song_meta
    meta, wait = {}, {}
    for t in held:
        c = components.candidates(songs[t], cfg.candidates, cfg.family, build=False)
        if len(held[t]) != len(c.cand):
            raise ValueError(f'{t}: {len(held[t])} saved probabilities for {len(c.cand)} candidates; the save and the '
                             f'cache disagree (audio or finder changed since the run)')
        meta[t] = song_meta(lab[t], c)
        tight = 'tol_s' in lab[t] or 'window_s' in lab[t]
        # The fixed wait from candidate to emission counts as lateness for families judged at the candidate.
        wait[t] = cfg.ahead_s + (float(np.median(c.emit_s - c.onset_s)) if tight and len(c.cand) else 0.0)
    per_song = {}
    held_out({t: lab[t] for t in held}, meta, held, log=None, per_song=per_song, ahead=wait, pool=pool)
    if partner is not None and coincident is not None:
        for t in held:
            if t in partner:
                s, stamp, avail, sr, hop = meta[t]
                coincident[t] = coincident_result(s, partner[t]['positives'], stamp, held[t], avail,
                                                  per_song[t]['cutoff'], sr, hop)
    return per_song


def migrate(meta):
    """A schema 1 meta (one net per run, npz keys are song names) as schema 2."""
    if meta.get('schema', 1) >= 2:
        return meta
    return dict(meta, schema=2, layers=['net'], final='net', groups=[n['group'] for n in meta['nets']],
                trainings=[dict(without=[k], held=n['group'], trained=n['trained'], clips=n['clips'])
                           for k, n in enumerate(meta['nets'])])


def layer_of(meta, z, layer):
    """(held {song: probabilities}, pool {song: probabilities to choose its cutoff on} or None) for one layer."""
    if meta['schema'] == 1:
        return {t: z[t] for t in meta['songs']}, None
    held = {t: z[f'{layer}__{t}'] for t in meta['songs']}
    grps = meta['groups']
    if not any(len(n['without']) == 2 for n in meta['trainings']):
        return held, None
    pool = {}
    for k, g in enumerate(grps):
        others = {u: z[f'pair{k}__{layer}__{u}'] for j, h in enumerate(grps) if j != k for u in h}
        for t in g:
            pool[t] = others
    return held, pool


def coincident_section(family, co, meta):
    """The report's coincident block: recall on the family's labels within COINCIDENT_S of a partner-family label,
    per song (exam and generated) and pooled over the exam songs the partner labels. None for a family without a
    partner or when the partner has no labels for any exam song."""
    if family not in PARTNER or not co:
        return None
    exam = [t for t in meta['exam'] if t in co]
    return dict(partner=PARTNER[family], window_ms=round(1000 * COINCIDENT_S), per_song=co,
                pooled=coincident_pooled(co, exam) if exam else None)


def score_run(run_id, layer=None, log=print):
    """Scores every saved seed of a run at a layer (default the final one); returns {seed: report}."""
    from tools.audio_analysis.lab.config import build
    from tools.audio_analysis.lab.registry import load
    d = RUNS / run_id
    raw = json.loads((d / 'meta.json').read_text())
    meta = migrate(raw)
    layer = layer or meta['final']
    if layer not in meta['layers']:
        sys.exit(f'{run_id}: no layer {layer}; it has {meta["layers"]}')
    cfg = build(meta['names'][0], meta['config'])
    lab, sha = labels(cfg.family)
    if sha != meta['labels_sha256']:
        sys.exit(f'{run_id}: the labels changed since this run trained; train it again')
    songs = load()
    partner = partner_labels(cfg.family)
    reports = {}
    for npz in sorted(d.glob('s*.npz')):
        t0 = time.time()
        seed = int(npz.stem[1:])
        with np.load(npz) as z:
            held, pool = layer_of(dict(meta, schema=raw.get('schema', 1)), z, layer)
            probes = meta.get('probes') or {}
            if probes and pool is not None:
                # A probe is the song at another gain, held out with it and judged at the cutoff the song gets at its
                # own level (its group's pool), so a level change that moves probabilities costs what it would live.
                from tools.audio_analysis.lab.registry import gain_copy
                for p, v in probes.items():
                    songs = dict(songs, **{p: gain_copy(songs[v['parent']], v['gain_db'], probe=True)})
                    lab = dict(lab, **{p: lab[v['parent']]})
                    held[p] = z[f'{layer}__{p}']
                    pool[p] = pool[v['parent']]
        co = {}
        ps = score(cfg, lab, songs, held, pool, partner=partner, coincident=co)
        timing = json.loads((d / f's{seed}.json').read_text())
        rep = dict(run_id=run_id, names=meta['names'], seed=seed, layer=layer, honest_cutoffs=pool is not None,
                   labels_sha256=sha, config=meta['config'],
                   sampling=meta['config']['sampling'], exam=meta['exam'], excluded=meta['excluded'],
                   recall_only=meta['recall_only'], results=dict(net=dict(per_song={t: ps[t] for t in meta['exam']})),
                   generated={t: ps[t] for t in meta['generated']},
                   probes={str(g): {v['parent']: ps[p] for p, v in probes.items() if v['gain_db'] == g}
                           for g in sorted({v['gain_db'] for v in probes.values()})},
                   coincident=coincident_section(cfg.family, co, meta),
                   timings=dict(build_s=timing['build_s'], train_s=timing['train_s'], score_s=round(time.time() - t0, 1)))
        name = f'report_s{seed}.json' if layer == meta['final'] else f'report_{layer}_s{seed}.json'
        (d / name).write_text(json.dumps(rep, indent=1, default=float))
        reports[seed] = rep
        log(f'{run_id} seed {seed}: scored in {time.time() - t0:.1f} s')
    return reports


def old_exam(old_id, exam):
    """(songs, header): the songs of run old_id's exam (its meta.json) that this exam also holds, and the one line
    saying which subset that is. Reads saved results only."""
    p = RUNS / old_id / 'meta.json'
    if not p.exists():
        sys.exit(f'--exam-of {old_id}: no such run (no {p})')
    old = json.loads(p.read_text())['exam']
    kept = [t for t in exam if t in set(old)]
    lost = [t for t in old if t not in set(exam)]
    head = f'old exam (run {old_id}): {len(kept)} of its {len(old)} songs are scored here, out of {len(exam)} in this exam'
    return kept, head + (f'; not in this exam: {lost}' if lost else '')


def old_exam_lines(reports, old_id):
    """Per seed, pooled recall, precision and F1 on the full exam and on the old exam's songs."""
    from tools.audio_analysis.eval.detector_compare import pooled
    first = next(iter(reports.values()))
    kept, head = old_exam(old_id, first['exam'])
    out = [head]
    for seed, r in sorted(reports.items()):
        ps, ro = r['results']['net']['per_song'], tuple(r['recall_only'])
        full, sub = pooled(ps, r['exam'], ro), pooled(ps, kept, ro) if kept else None
        out.append(f'  seed {seed}: full exam ({len(r["exam"])} songs) recall {full[0]:.3f} precision {full[1]:.3f} '
                   f'F1 {full[2]:.4f}' + (f' | old exam ({len(kept)} songs) recall {sub[0]:.3f} precision {sub[1]:.3f} '
                                          f'F1 {sub[2]:.4f}' if sub else ' | old exam: no songs in common'))
    return out


def legacy(npz, config, lab=None):
    """{song: song_result} for a save of the retired snare runner, on lab (the labels it was scored with; default
    today's); the scorer-parity oracle (test_scorer_parity)."""
    from tools.audio_analysis.lab.config import load as load_cfg
    from tools.audio_analysis.lab.registry import load
    cfg = load_cfg(config)
    lab = lab or labels(cfg.family)[0]
    with np.load(npz) as z:
        held = {t: z[t] for t in z.files}
    return score(cfg, lab, load(), held)


def main():
    a = sys.argv[1:]
    if a[:1] == ['--legacy']:
        t0 = time.time()
        out = Path(a[1]).with_suffix('.lab.json')
        out.write_text(json.dumps(dict(results=dict(net=dict(per_song=legacy(a[1], a[2])))), indent=1, default=float))
        print(f'-> {out} ({time.time() - t0:.1f} s)')
    elif a:
        reps = score_run(a[0], a[a.index('--layer') + 1] if '--layer' in a else None)
        if '--exam-of' in a and reps:
            print('\n'.join(old_exam_lines(reps, a[a.index('--exam-of') + 1])))
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main()
