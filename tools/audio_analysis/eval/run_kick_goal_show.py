#!/usr/bin/env python3
"""Scores the stacked kick detector (f69 + 8 s song-relative stage) on live-show recordings.

Usage: KICK_GOAL_MORE=1 KICK_GOAL_TRUTH=v3 run_kick_goal_show.py SONG LABELS.json [LABELS.json ...]

LABELS.json come from show_kick_labels.py for SONG's master. The detector is the
one the 18-song nested run used for SONG itself: base f69 model fitted without
SONG, stage fitted on the other songs' inner predictions, SONG's nested cutoff.
So the show takes are scored by a model that never saw SONG, and SONG's own
nested score is printed as the reference. Only scored bars count; the rest of
each recording (other songs, absent or masked bars) is an uncertain region, but
it still feeds the stage's causal history, as on stage.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, SUFFIX, TRUTH, Goal, add_whole_song_truth, counts, score  # noqa: E402
from tools.audio_analysis.eval.kick_goal_featsets import build, lowbank_cache, profile_cache  # noqa: E402
from tools.audio_analysis.eval.kick_goal_selfsim import self_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_templates import band_spec, patches, template_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_fusion_trial import training_labels  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_selfsim import fit2  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio, tail_cache  # noqa: E402

STAGE = 'R3_self8'
WINDOW_S = 8.0


def show_record(g, name, labels):
    rec_path = labels['recording']
    cache = GOAL / f'features_{name}.npz'
    if cache.exists():
        with np.load(cache) as z:
            cand, avail, feats, hop, dur, sr = z['candidates'], z['available'], z['features'], int(z['hop']), float(z['duration']), int(z['sr'])
    else:
        sr, x = read_audio(rec_path)
        cand, avail, feats, hop = fusion_features(x, sr)
        feats, dur = feats[:, :15], len(x) / sr
        np.savez(cache, candidates=cand, available=avail, features=feats, hop=hop, duration=dur, sr=sr)
    spans = sorted(tuple(s) for s in labels['scored_spans_s'])
    gaps, last = [], 0.0
    for a, b in spans:
        if a > last:
            gaps.append((last, a))
        last = max(last, b)
    gaps.append((last, dur + 1.0))
    src = dict(track=name, group='original_five', truth=list(labels['kicks_s']),
               regions=[dict(start_s=a, end_s=b, reason='not scored') for a, b in gaps])
    rec = dict(track=name, source=src, ref=None, sample_rate=sr, hop=hop, candidates=cand, available=avail,
               features=feats, duration=dur, removed=[], lag=0.0, all_labels=list(labels['kicks_s']),
               stem_kicks=None, ringing=None, free_spans=[])
    mask, y, _ = training_labels(src, None, cand, avail, sr, hop, dur)
    rec.update(training_mask=mask, labels=y, train_mask=mask, train_y=y,
               onset_s=(cand + 1) * hop / sr, emit_s=(avail + 1) * hop / sr)
    g.records[name] = rec
    g.labels['new'][name] = dict(mix_path=rec_path)
    tail_cache(g, name)
    path = GOAL / f'tmpl_{name}.npy'
    if not path.exists():
        np.save(path, template_features(patches(band_spec(mix_audio(g, name), sr, hop), cand, avail),
                                        np.load(GOAL / 'templates_from_dev.npy')))


def main():
    song, label_files = sys.argv[1], sys.argv[2:]
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    takes = []
    for f in label_files:
        labels = json.loads(Path(f).read_text())
        name = 'show_' + Path(labels['recording']).stem.split(' [')[0].replace(' ', '_').lower()
        show_record(g, name, labels)
        takes.append(name)
    build(g, 'f69')
    nested = np.load(GOAL / f'nested_f69{SUFFIX}.npz')
    cut = json.loads((GOAL / f'results_selfsim2_f69{SUFFIX}_{STAGE}.json').read_text())[STAGE]['all']['cutoffs'][song]
    fit, pred = gbt(g, True, 'f69')
    others = [u for u in ALL if u != song]
    base = fit(others)
    if not np.allclose(pred(base, song), nested[song], atol=1e-9):
        sys.exit('refit base model differs from the nested run: stale caches?')

    def stage_x(t, p):
        r = g.records[t]
        shape = np.hstack([profile_cache(g, t), lowbank_cache(g, t)])
        return self_features(p, shape, r['features'][:, 0], r['emit_s'], WINDOW_S)
    stage = fit2(g, {u: stage_x(u, nested[f'{song}|{u}']) for u in others}, others, list(range(5)))
    out = {}
    for t in [song] + takes:
        p = stage.predict_proba(stage_x(t, pred(base, t) if t != song else nested[song]))[:, 1]
        m, e, n = counts(score(g, t, p, cut)[1])
        out[t] = dict(matched=m, extra=e, labels=n, recall=round(m / max(1, n), 3), precision=round(m / max(1, m + e), 3))
        print(f'{t:20s} {m}/{n}+{e} R {out[t]["recall"]} P {out[t]["precision"]}', flush=True)
    (GOAL / f'results_show_{song}{SUFFIX}.json').write_text(json.dumps(dict(cutoff=cut, results=out), indent=1))


if __name__ == '__main__':
    main()
