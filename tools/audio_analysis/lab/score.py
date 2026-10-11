#!/usr/bin/env python3
"""Scores a run from its saves (docs/DETECTOR_LAB_DESIGN.md D7, D8): saved held-out probabilities, cached candidates
and the labels; no audio, no GPU. The rules are detector_eval.held_out's: each song at a cutoff chosen on the other
project songs, never on generated or recall-only songs. Writes report_s<seed>.json per seed: exam songs under
results.net.per_song (detector_compare's layout), generated songs in their own section.

Usage: score.py RUN_ID
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


def score(cfg, lab, songs, held):
    """{song: song_result} for every song in held, each at its held-out cutoff. A family with a tight hit window
    (labels' tol_s or window_s) is judged at the candidate, and its fixed wait to emission counts as lateness."""
    from tools.audio_analysis.eval.detector_eval import held_out
    from tools.audio_analysis.lab import components
    meta, wait = {}, {}
    for t in held:
        c = components.candidates(songs[t], cfg.candidates, cfg.family, build=False)
        tight = 'tol_s' in lab[t] or 'window_s' in lab[t]
        meta[t] = (lab[t], c.onset_s if tight else c.emit_s, c.avail, c.sr, c.hop)
        # The fixed wait from candidate to emission counts as lateness for families judged at the candidate.
        wait[t] = cfg.net['ahead_ms'] / 1000 + (float(np.median(c.emit_s - c.onset_s)) if tight and len(c.cand) else 0.0)
    per_song = {}
    held_out({t: lab[t] for t in held}, meta, held, log=None, per_song=per_song, ahead=wait)
    return per_song


def score_run(run_id, log=print):
    """Scores every saved seed of a run; returns {seed: report}."""
    from tools.audio_analysis.lab.config import Experiment
    from tools.audio_analysis.lab.registry import load
    d = RUNS / run_id
    meta = json.loads((d / 'meta.json').read_text())
    cfg = Experiment(name=meta['names'][0], seeds=(), **{**meta['config'], 'train': tuple(meta['config']['train'])})
    lab, sha = labels(cfg.family)
    if sha != meta['labels_sha256']:
        sys.exit(f'{run_id}: the labels changed since this run trained; train it again')
    songs = load()
    reports = {}
    for npz in sorted(d.glob('s*.npz')):
        t0 = time.time()
        seed = int(npz.stem[1:])
        with np.load(npz) as z:
            held = {t: z[t] for t in meta['songs']}
        ps = score(cfg, lab, songs, held)
        timing = json.loads((d / f's{seed}.json').read_text())
        rep = dict(run_id=run_id, names=meta['names'], seed=seed, labels_sha256=sha, config=meta['config'],
                   sampling=meta['config']['sampling'], exam=meta['exam'], excluded=meta['excluded'],
                   recall_only=meta['recall_only'], results=dict(net=dict(per_song={t: ps[t] for t in meta['exam']})),
                   generated={t: ps[t] for t in meta['generated']},
                   timings=dict(build_s=timing['build_s'], train_s=timing['train_s'], score_s=round(time.time() - t0, 1)))
        (d / f'report_s{seed}.json').write_text(json.dumps(rep, indent=1, default=float))
        reports[seed] = rep
        log(f'{run_id} seed {seed}: scored in {time.time() - t0:.1f} s')
    return reports


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
        score_run(a[0])
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main()
