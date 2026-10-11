#!/usr/bin/env python3
"""The A/B of two experiments (docs/DETECTOR_LAB_DESIGN.md D6, D8): trains whatever seeds are missing, A and B
interleaved so a slow machine hour hurts both alike, scores both from their saves, refuses to compare runs whose exam
songs or labels differ, and prints the exam A/B (detector_compare), then the generated songs on their own. The text
is also written to the lab cache's compare/ folder. Configs with one net per song are refused: they cannot choose a
song's cutoff on models that never saw it.

Usage: compare.py CONFIG_A CONFIG_B [--seeds 0,1,2] [--layer net|trees|combined]   (layer: default each final)"""
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

from tools.audio_analysis.lab.run import RUNS, run, run_id_of  # noqa: E402


def check(a, b):
    """Two runs' reports compare only on the same exam songs and the same labels."""
    for k in ('labels_sha256', 'exam', 'recall_only'):
        if a[k] != b[k]:
            raise ValueError(f'{k} differs between {a["run_id"]} and {b["run_id"]}: not a fair A/B')


def robustness(reps, recall_only, out):
    """Level robustness (sampling.gain_probes): pooled F1 and false fires a minute over the exam, seeds averaged, at
    each probed gain and at the song's own level (0 dB), every song at the cutoff it gets at its own level."""
    from tools.audio_analysis.eval.detector_compare import pooled
    exam = reps[0]['exam']
    gains = sorted(reps[0]['probes'], key=float)
    out('level robustness (exam songs scaled by the gain, cutoff from the song at its own level):')
    out(f'  {"gain dB":>8s} {"F1":>6s} {"recall":>7s} {"precision":>9s} {"false/min":>9s}')
    for g in sorted(gains + ['0'], key=float):
        rows = []
        for r in reps:
            ps = r['results']['net']['per_song'] if g == '0' else r['probes'][g]
            rec, prec, f1 = pooled(ps, exam, tuple(recall_only))
            f = sum(ps[t]['extra'] for t in exam if t not in recall_only)
            mins = sum(ps[t]['minutes'] for t in exam if t not in recall_only)
            rows.append((f1, rec, prec, f / mins))
        m = [sum(v) / len(rows) for v in zip(*rows)]
        out(f'  {float(g):>+8.1f} {m[0]:6.3f} {m[1]:7.3f} {m[2]:9.3f} {m[3]:9.2f}')


def main():
    from tools.audio_analysis.eval.detector_compare import compare
    from tools.audio_analysis.lab.config import load
    from tools.audio_analysis.lab.score import score_run
    a = sys.argv[1:]
    if len(a) < 2:
        sys.exit(__doc__)
    cfgs = [load(a[0]), load(a[1])]
    for c in cfgs:
        if not c.folds:
            sys.exit(f'{c.name}: folds 0 (one net per song) has optimistic cutoffs; compare runs with song groups')
    layer = a[a.index('--layer') + 1] if '--layer' in a else None
    seeds = [int(s) for s in a[a.index('--seeds') + 1].split(',')] if '--seeds' in a else sorted(set(cfgs[0].seeds) & set(cfgs[1].seeds))
    ids = [run_id_of(a[0]), run_id_of(a[1])]
    todo = [(k, s) for s in seeds for k in (0, 1) if not (RUNS / ids[k] / f's{s}.npz').exists()]
    t0 = time.time()
    for n, (k, s) in enumerate(todo):
        run(a[k], [s])
        left = len(todo) - n - 1
        if left:
            per = (time.time() - t0) / (n + 1)
            print(f'{left} runs left, finish about {time.strftime("%H:%M", time.localtime(time.time() + left * per))}', flush=True)
    reps = [score_run(i, layer, log=lambda m: None) for i in ids]
    for k in (0, 1):
        missing = set(seeds) - set(reps[k])
        if missing:
            sys.exit(f'{ids[k]} has no seeds {sorted(missing)}')
    check(reps[0][seeds[0]], reps[1][seeds[0]])
    lines = []

    def out(m):
        lines.append(m)
        print(m)
    for k, c in enumerate(cfgs):
        r = reps[k][seeds[0]]
        out(f'{"AB"[k]}: {c.name} (run {ids[k]}, layer {r["layer"]}) sampling {r["sampling"]}, train {list(c.train)}, '
            f'train {sum(reps[k][s]["timings"]["train_s"] for s in seeds) / 60:.1f} min over {len(seeds)} seeds')
    first = reps[0][seeds[0]]
    out(f'exam: {len(first["exam"])} project songs, seeds {seeds}, excluded by audit {first["excluded"]}')
    compare([[reps[k][s]['results']['net']['per_song'] for s in seeds] for k in (0, 1)], tuple(first['recall_only']), out)
    if first['generated']:
        out('generated songs (never set a cutoff; trained on only where the config trains them):')
        compare([[reps[k][s]['generated'] for s in seeds] for k in (0, 1)], (), out)
    for k, c in enumerate(cfgs):
        if reps[k][seeds[0]].get('probes'):
            out(f'{"AB"[k]}: {c.name}')
            robustness([reps[k][s] for s in seeds], first['recall_only'], out)
    d = RUNS.parent / 'compare'
    d.mkdir(parents=True, exist_ok=True)
    path = d / f'{cfgs[0].name}_vs_{cfgs[1].name}.txt'
    path.write_text('\n'.join(lines) + '\n')
    print(f'-> {path}')


if __name__ == '__main__':
    main()
