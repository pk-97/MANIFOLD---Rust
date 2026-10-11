#!/usr/bin/env python3
"""The kick detector's verdict at each snare candidate, a snare input (Peter, 2026-10-11: snares fire on bright kicks;
a kick and snare together still count as a snare, so the snare also keeps its own upper-band evidence).

The verdict is the shipped kick release's final probability at its own candidates. On songs the kick trained on it is
the kick run's held-out probability for the release seed, so no song's verdict comes from a model that saw its kick
labels; on every other song it is the release itself. It is built in its own process (the kick release pins its own
net env) into one store per kick release; the snare measure only reads the store.

Usage: kick_verdict.py build"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402

STORE = Path.home() / '.cache/manifold/detector-lab/kick_verdict'
BEFORE_S, AFTER_S = .040, .020   # a kick onset this far before or after the snare onset is the same moment
NONE_DT_MS = 60.0                # dt when no kick verdict is in the window


def kick_release():
    from tools.audio_analysis.lab.release import asset_train_id
    return asset_train_id('kick')


def store_path(train_id):
    return STORE / f'{train_id}.npz'


def build():
    """Every snare-family song's kick verdict under the shipped kick release."""
    from tools.audio_analysis.lab import components, models, release, run
    tid = kick_release()
    rec = release.load(tid)
    kcfg = release.use(rec['config'], tid)
    klab, songs, kmeta = run.setup(kcfg)
    z = dict(np.load(run.RUNS / rec['run_id'] / f"s{rec['seed']}.npz"))
    held = {}
    for n in kmeta['trainings']:
        if len(n['without']) == 1:
            for t, pre in models.save_keys(kmeta['groups'], n).items():
                k = f'{pre}__staged__{t}' if pre else f'staged__{t}'
                if k in z:
                    held[t] = z[k]
    from tools.audio_analysis.lab.registry import mix
    out = {}
    for t, song in songs.items():
        if 'snare' not in song.families:
            continue
        if t in klab:
            if t not in held:
                sys.exit(f'{t}: the kick run {rec["run_id"]} saved no held-out probability')
            c = components.candidates(song, kcfg.candidates, kcfg.family, build=False)
            p, how = held[t], 'held out'
        else:
            r = release.reference(rec, mix(song), sr=song.sample_rate)
            c, p, how = r['c'], r['p_final'], 'release'
        out[f'{song.content_id}.onset'] = np.asarray(c.onset_s, np.float64)
        out[f'{song.content_id}.emit'] = np.asarray(c.emit_s, np.float64)
        out[f'{song.content_id}.p'] = np.asarray(p, np.float64)
        print(f'{t:18s} {how:8s} {len(c.cand)} kick candidates', flush=True)
    STORE.mkdir(parents=True, exist_ok=True)
    tmp = store_path(tid).with_suffix('.tmp.npz')
    np.savez(tmp, **out)
    tmp.replace(store_path(tid))
    print(f'{len(out) // 3} songs -> {store_path(tid)}')


def features(song, c):
    """(n snare candidates, 2): the most confident kick verdict whose onset sits within BEFORE_S before to AFTER_S
    after the snare onset and that is out by the snare's own emission (so it adds no wait), and that kick's onset
    minus the snare's in ms (NONE_DT_MS with no kick)."""
    tid = kick_release()
    path = store_path(tid)
    if not path.exists():
        raise SystemExit(f'no kick verdict store for {tid}: run lab/kick_verdict.py build')
    z = np.load(path)
    if f'{song.content_id}.p' not in z:
        raise SystemExit(f'{song.id}: not in the kick verdict store {path.name}: run lab/kick_verdict.py build')
    on, emit, p = (z[f'{song.content_id}.{k}'] for k in ('onset', 'emit', 'p'))
    return features_from(on, emit, p, c)


def features_from(on, emit, p, c):
    """features() on kick decisions given as onset and emission times (s, emission order) and final
    probabilities."""
    on, emit, p = (np.asarray(a, np.float64) for a in (on, emit, p))
    out = np.zeros((len(c.cand), 2))
    out[:, 1] = NONE_DT_MS
    lo = np.searchsorted(on, c.onset_s - BEFORE_S, 'left')
    hi = np.searchsorted(on, c.onset_s + AFTER_S, 'right')
    for i in range(len(c.cand)):
        j = np.arange(lo[i], hi[i])
        j = j[emit[j] <= c.emit_s[i] + 1e-9]
        if len(j):
            b = j[np.argmax(p[j])]
            out[i] = p[b], 1e3 * (on[b] - c.onset_s[i])
    return out


if __name__ == '__main__':
    if sys.argv[1:] != ['build']:
        sys.exit(__doc__)
    build()
