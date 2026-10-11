#!/usr/bin/env python3
"""Bass and synth note detection on Peter's clean labels project (2026-10-11): four sections at 140 BPM, each one lead
(beepy, reverb, pluck, supersaw), one bass (laser, FM wobble, dubstep wub, growl) and a LinnDrum pattern, with MIDI
note starts as exact truth and stems plus the full mix exported from the same arrangement.

Per family: candidates (rise 3 dB or spectral change 1.5 in the family band, detector_net 'union:3:1.5'), measured
with the kick measurements that work at any candidate (rise and fall profiles, low filter bank, residual, pitch
glide: 83) and pitch measurements (note_pitch), scored by a gradient-boosted tree trained on three sections and
tested on the fourth (an unseen sound), cutoff chosen on the other sections' held-out scores. A note is caught by
a candidate within 30 ms; lateness is the decision (emission) minus the note.
Usage: note_clean.py FAMILY mix|stem [kick|pitch|all|none]   (none: fire on every candidate)"""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

PROJECT = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects/Other And Backups/Labels and Timings Project')
EXPORTS = {'mix': 'FULL MIX', 'stem': {'bass': 'BASS', 'synth': 'LEADS'}}
SR = 48000
TOL, LAB_TOL, REFR = .030, .035, .060


def truth(res, part):
    """{section track: sorted note starts (s, export time)} for the family's tracks, layers within 30 ms merged. The
    exports start at the first clip (27.43 s, bar 17), not at zero: notes are shifted by it (lt_align: drums, leads
    and bass attacks all line up within 70 ms of that shift and nowhere near zero)."""
    from tools.audio_analysis.eval.part_classes import classify
    start = min(c['start_s'] for tr in res['tracks'] for c in tr['midi_clips'])
    out = {}
    for tr in res['tracks']:
        if classify(tr['name'], tr.get('groups') or ()) != part:
            continue
        t = np.sort([n['s'] for c in tr['midi_clips'] for n in c['notes'] if n['probability'] >= 1])
        if len(t):
            out[tr['name']] = t[np.concatenate([[True], np.diff(t) > .030])] - start
    return out


def measurements(x, cand, avail, which):
    from tools.audio_analysis.eval.detector_cands import HOP
    from tools.audio_analysis.eval.kick_glide_features import glide_features
    from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes, lowbank_features
    from tools.audio_analysis.eval.kick_goal_profile import fall_profile, rise_profile
    from tools.audio_analysis.eval.kick_goal_residual import residual_features
    from tools.audio_analysis.eval.kick_goal_templates import band_spec
    from tools.audio_analysis.eval.note_pitch import pitch_features
    onset, emit = (cand + 1) * HOP / SR, (avail + 1) * HOP / SR
    cols = []
    if which in ('kick', 'all'):
        spec = band_spec(x, SR, HOP)
        env = band_envelopes(x, SR)
        cols += [rise_profile(spec, cand, avail), fall_profile(spec, cand, avail), lowbank_features(env, onset, emit),
                 residual_features(env, onset, emit), glide_features(x, SR, HOP, cand, avail)]
    if which in ('pitch', 'all'):
        cols.append(pitch_features(x, SR, onset, emit))
    return np.hstack(cols) if cols else np.zeros((len(cand), 0))


def fires(p, avail, th):
    from tools.audio_analysis.eval.detector_cands import HOP
    out, last = [], -1e9
    for i in np.flatnonzero(p >= th):
        if (avail[i] - last) * HOP / SR >= REFR:
            out.append(i)
            last = avail[i]
    return np.array(out, int)


def judge(notes, onset, emit, p, avail, th):
    """(caught, notes, false, lateness ms of caught) for one section."""
    f = fires(p, avail, th)
    if not len(f):
        return 0, len(notes), 0, []
    d = np.abs(onset[f][:, None] - notes[None, :])
    caught = d.min(0) <= TOL
    false = int(np.sum(d.min(1) > TOL))
    late = [1000 * (emit[f][np.argmin(d[:, k])] - notes[k]) for k in np.flatnonzero(caught)]
    return int(caught.sum()), len(notes), false, late


def main():
    from sklearn.ensemble import HistGradientBoostingClassifier
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_cands import HOP
    from tools.audio_analysis.eval.detector_labels import read_any
    from tools.audio_analysis.eval.detector_net import family_candidates
    from tools.audio_analysis.eval.kick_goal_eval import load
    from tools.audio_analysis.eval.kick_goal_nn import spectrum
    import importlib
    family, audio_kind = sys.argv[1], sys.argv[2]
    which = sys.argv[3] if len(sys.argv) > 3 else 'all'
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    res = extract(PROJECT / 'Labels and Timings.als')
    notes = truth(res, fam.part)
    name = EXPORTS['mix'] if audio_kind == 'mix' else EXPORTS['stem'][family]
    x = read_any(str(PROJECT / 'Exports' / f'Labels and Timings - V1 {name}.wav'), SR, load)
    cand, avail = family_candidates('union:3:1.5', fam)(x, SR, spectrum(x, SR))
    onset, emit = (cand + 1) * HOP / SR, (avail + 1) * HOP / SR
    X = measurements(x, cand, avail, which)
    every = np.sort(np.concatenate(list(notes.values())))
    y = (np.abs(onset[:, None] - every[None, :]).min(1) <= LAB_TOL).astype(int)
    # Sections: each from a second before its first note to a second before the next section's first note.
    starts = sorted((v[0], k) for k, v in notes.items())
    edges = [0.0] + [s - 1.0 for s, _ in starts[1:]] + [len(x) / SR]
    sec = np.clip(np.searchsorted(edges, onset, side='right') - 1, 0, len(starts) - 1)
    span = list(zip(edges[:-1], edges[1:]))
    held = np.zeros(len(cand))
    for k in range(len(starts)):
        test = sec == k
        if which == 'none':
            held[test] = 1.0
            continue
        w = np.where(y[~test] == 1, .5 / max(1, y[~test].sum()), .5 / max(1, (1 - y[~test]).sum()))
        m = HistGradientBoostingClassifier(max_iter=200, learning_rate=.05, max_leaf_nodes=15, min_samples_leaf=40,
                                           l2_regularization=1.0, random_state=0).fit(X[~test], y[~test], sample_weight=w * len(w))
        held[test] = m.predict_proba(X[test])[:, 1]
    print(f'{family} {audio_kind} {which}: {len(cand)} candidates ({len(cand) / (len(x) / SR):.1f}/s), {X.shape[1]} measurements, '
          f'{int(y.sum())} on a note of {len(every)}')
    tot = np.zeros(3)
    lates = []
    for k, (s0, track) in enumerate(starts):
        nk = notes[track]
        others = [j for j in range(len(starts)) if j != k]
        best = (-1, .5)
        for th in np.linspace(.05, .99, 95) if which != 'none' else [.5]:
            m_ = n_ = f_ = 0
            for j in others:
                a, b, c, _ = judge(notes[starts[j][1]], onset[sec == j], emit[sec == j], held[sec == j], avail[sec == j], th)
                m_, n_, f_ = m_ + a, n_ + b, f_ + c
            r, pr = m_ / max(1, n_), m_ / max(1, m_ + f_)
            best = max(best, (2 * r * pr / max(1e-9, r + pr), th))
        a, b, c, late = judge(nk, onset[sec == k], emit[sec == k], held[sec == k], avail[sec == k], best[1])
        mins = (span[k][1] - span[k][0]) / 60
        tot += (a, b, c)
        lates += late
        print(f'  {track:22s} caught {a:3d}/{b:3d} ({a / max(1, b):.0%}), {c / mins:5.1f} false per minute, '
              f'late median {np.median(late) if late else float("nan"):.0f} ms, cutoff {best[1]:.2f}')
    r, pr = tot[0] / tot[1], tot[0] / max(1, tot[0] + tot[2])
    print(f'  all: recall {r:.2f} precision {pr:.2f} F1 {2 * r * pr / max(1e-9, r + pr):.3f}, late median {np.median(lates):.0f} ms')


if __name__ == '__main__':
    main()
