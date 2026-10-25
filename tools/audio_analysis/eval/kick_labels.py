#!/usr/bin/env python3
"""Kick labels in the lab format: GOAL/kick_labels.json for the kick goal set, the
songs the shipped v1 kick trained on. The truth is the kick goal records' own, never re-derived: Goal(mode='v3') after
add_whole_song_truth (the whole-song kick-stem truth where a song has its own stem, else the reviewed passages, the
trigger kicks or the project sections). Per song:

- positives: the training truth; unscored: everything the record does not train on, as onset spans (outside the
  passages, the excluded regions widened by the finder's fixed wait, a window round each ignored kick), then kick
  rolls (detector_labels.roll_spans, Peter's rolls ruling) with the kicks inside them dropped;
- hard_neg: melodic note starts more than 105 ms from every kick (kick_goal_melodic's 70 ms plus the 35 ms row
  window), so rows() needs no kick rule;
- proof: 'stem' (kicks measured on the song's own kick stem: no project offset needed), 'trigger' (the trigger
  track's verified kicks), 'reviewed' (Peter's reviewed passages) or 'project' (project MIDI; the song's offset proof
  decides).

Gate: before rolls, detector_eval.rows(labels, the record's candidates) equals the record's (train_mask, train_y) for
every song, or nothing is written.

`project` writes GOAL/kick_labels_project.json for the catalogue's label-ready songs (detector_wip.PAIRS with a proven
offset) and the project-folder songs in PROJECT_KICKS by the shared labeller's rules (detector_labels, PROJECT): kick notes on playing tracks and pads and placed kick
one-shots are the positives, kept where the mix has a kick-band attack; muted drums, unnamed drum pads and drum audio
are unknown; rolls are unscored; melodic notes and other drum voices more than 105 ms from every kick note are vouched
negatives. The offset proof is the song's own (detector_wip).

Usage: kick_labels.py [project]"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.detector_eval import LAB_TOL, rows  # noqa: E402

GAP_HOPS = 8          # the kick finder's emission comes this many hops after its candidate
HARD_FAR_S = .105     # vouched negatives: melodic notes this far from every kick
# detector_wip.PROJECTS songs whose kicks are labelled too. Cadence (173 kick notes, BUG-fffrv (album and EP songs)) joins
# when its offset is corrected: built now, its kick labels sit 9 ms after the attacks and its snares 11 ms, and
# fix/label-timing-2026-10-13 (BUG-a2pma (labels timing_shift)) moves its offset by -12.4 ms.
PROJECT_KICKS = ()
EPS = 1e-9


def passages(r):
    """The record's training passages: (start, end, truth, excluded regions), as run_kick_fusion_trial.training_labels
    reads them."""
    src = r.get('train_source', r['source'])
    if src['group'] == 'original_five':
        dur = r['duration'] or (r['available'][-1] + 64) * r['hop'] / r['sample_rate']
        return [(0.0, dur, list(src['truth']), src['regions'])]
    return [(p['start_s'], p['end_s'], list(p['kick_times_s']), s['excluded_regions'])
            for p, s in zip(src['passages'], r['ref']['scores']['v5'])]


def merged(spans, dur):
    """Spans merged and clipped to the song, so scored minutes count each second once."""
    out = []
    for a, b in sorted((max(0.0, a), min(dur, b)) for a, b in spans):
        if b < a:
            continue
        if out and a <= out[-1][1]:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def song_labels(r):
    """(labels before rolls, every kick time the record knows) for one record."""
    gap = GAP_HOPS * r['hop'] / r['sample_rate']
    ps = sorted(passages(r), key=lambda p: p[0])
    unscored, pos = [], []
    edge = 0.0
    for start, end, truth, excluded in ps:
        unscored.append([edge, start - EPS])
        # An emission landing exactly on a boundary is out (legacy: emit < end, emit >= region start); EPS keeps
        # float rounding on that side. Candidates sit 5 ms apart, so EPS moves no other row.
        edge = end - gap - EPS
        for e in excluded:
            unscored.append([e['start_s'] - gap - EPS, e['end_s']])
        for u in truth:
            ignored = u < start or u >= end or any(e['start_s'] <= u <= e['end_s'] for e in excluded)
            if ignored:
                unscored.append([u - LAB_TOL - 1e-12, u + LAB_TOL + 1e-12])
            else:
                pos.append(float(u))
    dur = float((r['available'][-1] + 64) * r['hop'] / r['sample_rate'])
    unscored.append([edge, dur])
    kicks = r['stem_kicks'] if r.get('stem_kicks') is not None else r.get('all_labels', pos)
    return dict(positives=sorted(pos), unscored=merged(unscored, dur), loop_spans=[], hard_neg=[], duration_s=dur,
                positives_only=False), np.sort(np.asarray(kicks, float))


def proof(t, r, g):
    from tools.audio_analysis.eval.kick_goal_eval import STEM_CFG
    kind = g.labels['new'].get(t, {}).get('kind')
    if kind in ('trigger', 'sections'):
        return dict(trigger='trigger', sections='project')[kind]
    return 'stem' if 'train_source' in r or t in STEM_CFG else 'reviewed'


def build(write=True):
    """{song: labels}; written to GOAL/kick_labels.json when write."""
    from tools.audio_analysis.eval.detector_eval import inside, nearest
    from tools.audio_analysis.eval.detector_labels import roll_spans
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal, add_whole_song_truth
    from tools.audio_analysis.eval.kick_goal_melodic import melodic_onsets
    g = Goal(mode='v3')
    add_whole_song_truth(g)
    out, bad = {}, []
    for t, r in g.records.items():
        s, kicks = song_labels(r)
        onset = (np.asarray(r['candidates']) + 1) * r['hop'] / r['sample_rate']
        mask, y, _ = rows(s, onset)
        # y only matters on trained rows: the legacy rule leaves every other row at 0.
        ty = r['train_y'] == 1
        if not (np.array_equal(mask, r['train_mask']) and np.array_equal((y == 1) & mask, ty & mask)):
            diff = np.flatnonzero((mask != r['train_mask']) | (((y == 1) != ty) & mask))
            bad.append(f"{t}: mask differs on {int(np.sum(mask != r['train_mask']))}, y on "
                       f"{int(np.sum(((y == 1) != ty) & mask))} of {len(onset)} candidates; first at "
                       + ', '.join(f'{onset[i]:.4f} s (mask {mask[i]}/{r["train_mask"][i]}, y {y[i]:.0f}/{ty[i]:d})' for i in diff[:3]))
            continue
        notes = melodic_onsets(t) + r['lag']
        s['hard_neg'] = sorted(float(u) for u in notes[nearest(notes, kicks) > HARD_FAR_S]) if len(notes) else []
        rolls = roll_spans(kicks)
        pos = np.array(s['positives'])
        gone = inside(pos, rolls) if len(pos) else np.zeros(0, bool)
        s['positives'] = [float(u) for u in pos[~gone]]
        s['dropped'] = [float(u) for u in pos[gone]]
        s['unscored'] = merged(s['unscored'] + [[float(a), float(b)] for a, b in rolls], s['duration_s'])
        s['positives_only'] = bool(r.get('positives_only'))
        s['proof'] = proof(t, r, g)
        s['offset_s'], s['kind'] = None, 'project'
        out[t] = s
    if bad:
        sys.exit('rows differ from the kick records; nothing written:\n' + '\n'.join(bad))
    if not write:
        return out
    path = GOAL / 'kick_labels.json'
    path.write_text(json.dumps(out) + '\n')
    n = sum(len(s['positives']) for s in out.values())
    d = sum(len(s['dropped']) for s in out.values())
    print(f'{len(out)} songs, {n} kicks ({d} in rolls left out) -> {path}')
    return out


def project_family():
    """The kick as a detector_labels family, for project songs labelled from their sets."""
    import re

    from tools.audio_analysis.eval.detector_labels import Family
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.detector_wip import PAIRS
    return Family(
        name='kick',
        voice=re.compile(r'kick|kik\b|\bbd\b|bass ?drum', re.I),
        exclude=re.compile(r'(?!)'),
        other_voice=re.compile(r'snare|snr|clap|clp|rim|xstick|cross|hat|\bhh|ride|cymbal|crash|perc|shaker|shkr|tamb|tom|'
                               r'conga|bongo|cowbell|clave|wood|block|triangle|guiro|cabasa|snap', re.I),
        # Drum audio and unclassed audio may hold a kick (detector_labels.project_parts).
        other_parts=re.compile(r'drum|break|_top|top_?loop|perc|groove|fill|beat|kit', re.I),
        not_other_parts=re.compile(r'vocal|vox|synth|string|pad|chord', re.I),
        band=(100, 400),       # the kick's punch: below 100 Hz the sub and bass hide its rise
        stem_band=(40, 10000),
        dense_s=0.0,           # a double or triple kick stays a hit; only rolls are unscored
        drop_kick_rolls=True,
        far_s=HARD_FAR_S,
        own_kicks=True,
        add_wip=False,
        midi_songs=tuple(t for t in (*PAIRS, *PROJECT_KICKS) if t in wip()),
    )


def build_project():
    from tools.audio_analysis.eval.detector_labels import write
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    return write(project_family(), GOAL / 'kick_labels_project.json', lambda m: print(m, flush=True))


if __name__ == '__main__':
    if sys.argv[1:] == ['project']:
        build_project()
    elif sys.argv[1:]:
        sys.exit(__doc__)
    else:
        build()
