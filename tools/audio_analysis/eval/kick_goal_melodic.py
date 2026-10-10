"""Certain non-kicks from the project: melodic note starts (bass, subs, plucks, leads).

A candidate within NOTE_MS of a melodic MIDI note start and more than KICK_FAR_S
from every kick label is a non-kick the project vouches for: the bass and pluck
attacks the detector confuses with kicks (Right Where's bassline, Facade's plucks).
add_melodic_negatives marks them; training weighs them HARD_W times a plain
non-kick (kick_goal_eval.training_set, kick_goal_nn.train). Training only: the
detector never sees a note.

Melodic tracks: every MIDI track with notes whose name is not a drum, trigger,
FX or Resolume track. Song time = project seconds + the song's export offset
(trigger, WIP and recall songs store it; stem songs refit it from the kick stem
as kick_goal_project_kicks does) + the record's lag.
"""
from __future__ import annotations

import json
import re

import numpy as np

from tools.audio_analysis.eval.als_extract import extract
from tools.audio_analysis.eval.kick_goal_labels import GOAL
from tools.audio_analysis.eval.kick_goal_project_kicks import PROJECT
from tools.audio_analysis.eval.kick_goal_trigger_labels import TRIGGER, export_offset, note_times
from tools.audio_analysis.eval.kick_goal_wip_labels import SURVEY

NOTE_MS = 35
KICK_FAR_S = .07
HARD_W = 4.0
NOT_MELODIC = re.compile(r'kick|snare|drum|hat|ride|perc|trigger|clap|resolume|crash|cymbal|tom|shaker|tamb|fx|riser|impact|'
                         r'linndrum|break|loop', re.I)


def _source(name):
    """(als path, export offset s) for a song with a project, else None."""
    if name in TRIGGER:
        info = json.loads((GOAL / 'labels_trigger.json').read_text())['new'][name]
        return TRIGGER[name]['als'], info['export_offset_s']
    if (SURVEY / f'{name}.json').exists():
        s = json.loads((SURVEY / f'{name}.json').read_text())
        return s['als'], s['offset_s']
    if name in PROJECT:
        res = extract(PROJECT[name]['als'])
        stem = np.sort(np.load(GOAL / f'notes_{name}.npy'))
        return PROJECT[name]['als'], export_offset(stem, note_times(res, PROJECT[name]['kick']))
    return None


def melodic_onsets(name):
    """Melodic note starts in export time (s), cached per song; empty without a project."""
    path = GOAL / f'melodic_{name}.npy'
    if not path.exists():
        src = _source(name)
        times, used = np.zeros(0), []
        if src is not None:
            res = extract(src[0])
            for t in res['tracks']:
                notes = [n['s'] for c in t['midi_clips'] for n in c['notes']]
                if notes and not NOT_MELODIC.search(t['name']):
                    used.append(t['name'])
                    times = np.concatenate([times, notes])
            times = np.unique(np.round(times, 4)) + src[1]
        print(f'{name}: {len(times)} melodic note starts from {len(used)} tracks', flush=True)
        np.save(path, times)
    return np.load(path)


def add_melodic_negatives(g):
    """Marks each record's project-vouched non-kick candidates in r['hard_neg']."""
    for t, r in g.records.items():
        notes = melodic_onsets(t) + r['lag']
        kicks = r['stem_kicks'] if r.get('stem_kicks') is not None else r.get('all_labels', r['source'].get('truth', []))
        kicks = np.sort(np.asarray(kicks, float))
        on = np.asarray(r['onset_s'])
        hard = np.zeros(len(on), bool)
        if len(notes):
            j = np.clip(np.searchsorted(notes, on), 1, len(notes) - 1)
            near_note = np.minimum(np.abs(on - notes[j - 1]), np.abs(on - notes[j])) <= NOTE_MS / 1000
            if len(kicks):
                k = np.clip(np.searchsorted(kicks, on), 1, len(kicks) - 1)
                far = np.minimum(np.abs(on - kicks[k - 1]), np.abs(on - kicks[k])) > KICK_FAR_S
            else:
                far = np.ones(len(on), bool)
            hard = near_note & far
        r['hard_neg'] = hard
