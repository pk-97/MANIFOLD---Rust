"""Kick-stem truth confirmed by the project's kick trigger track.

Peter, 2026-10-10: the real kicks are the ones with a real attack that line up
with something in the project; a custom detector does not decide what a kick is.
A stem attack (kick_goal_rolls.kick_notes) is a kick only when a note of the
project's kick track lands within CONFIRM_MS of it. Attacks without a note are not
kicks: Midnight Patience's kick runs through a delay, so 198 of its 320 stem
attacks were echoes at +0.5 and +1.25 beats. The attack's own time is kept, so
the label sits on the audio, not on the note.

The export offset is kick_goal_trigger_labels.export_offset: the candidate
offset that puts the most notes on an attack.
"""
from __future__ import annotations

from pathlib import Path

import numpy as np

from tools.audio_analysis.eval.als_extract import extract
from tools.audio_analysis.eval.kick_goal_labels import ABL, GOAL, load
from tools.audio_analysis.eval.kick_goal_trigger_labels import export_offset, nearest, note_times

# A note confirms an attack within CONFIRM_MS: Heavy on Mind and Late Night have real
# kicks 21-28 ms off their notes; delay echoes sit a quarter beat or more away.
CONFIRM_MS = 35
QUIET_DB = 10
LOUDER_DB = 4
ECHO_S = .2
# Closer pairs are flams, merged into one kick by kick_goal_eval.one_per_refractory.
REFRACTORY_S = .06

# Drum stems with no drums entry in kick_goal_labels. Gerrit's room and tom mics are
# left out: their kick-shaped hits are toms, which are not kicks.
OTHER_DRUMS = {
    'gerrit': ('Drums overhead.wav',),
    'worship': ('Worship Stems - Bus 1-24b.wav', 'Worship Stems - Bus 2-24b.wav',
                'Worship Stems - KSHMR Acoustic Fill 128BPM 03-24b.wav'),
}

# Projects found by fitting every kick track in Peter's 2024-2025 projects to each
# stem (2026-10-10 scan). Late Night's kick also runs through a delay: 20 quiet
# quarter-beat echoes before its drops have no note.
PROJECT = {
    'midnight_patience': dict(als=ABL / '2024/Bettys Project/Bettys (Midnight Patience) - Export.als', kick='43-DS Kick'),
    'late_night': dict(als=ABL / '2024/Late Night - STEM MASTER Project/L12 Meld + Roar (Late Night) - Master FLAT Final.als',
                       kick='29-DS Kick'),
    'miracle': dict(als=ABL / '2024/Streets Project/Streets - FINALS.als', kick='38-DS Kick'),
    'heavy_on_mind': dict(als=ABL / '2024/Texture Project/Texture - APR Feedback - DESS - EXPORTS.als', kick='62-DS Kick'),
    'pattern': dict(als=ABL / '2025/Pattern Project/Pattern - Master (Premaster Fix).als', kick='40-KICK'),
    'back_to_you': dict(als=ABL / '2024/States Project/States (Back to You) - MASTER FINAL V3.als', kick='48-DS Kick'),
    'burn_stems': dict(als=ABL / '2025/Fifty Project/Fifty (Burn) - PREMASTER T2.als', kick='69-DS Kick'),
}


def other_drum_stems(track, cfg):
    """The stems besides the kick stem that may carry a kick the kick stem does not:
    the configured drums stem, plus OTHER_DRUMS. A kick-shaped hit there far from
    every label is left unscored, as on the trigger songs."""
    extra = [Path(cfg['parts_dir']) / f for f in OTHER_DRUMS.get(track, ())] if cfg.get('parts_dir') else []
    return list(cfg.get('drums', [])) + extra


def doubtful(track, kick, attacks, sr):
    """Songs without a project: attacks that may be an echo or a ghost hit, left unscored
    (cached per track).
    One is at least QUIET_DB under the song's median attack and within ECHO_S of an attack
    LOUDER_DB louder. Quiet kicks on the beat (Cold's and Eat Sleep's quiet sections)
    and flams (one_per_refractory) are not caught."""
    path = GOAL / f'doubt_{track}.npy'
    if path.exists():
        return np.load(path)
    paths = kick if isinstance(kick, list) else [kick]
    xs = [np.abs(load(p, sr)) for p in paths]
    n = min(len(x) for x in xs)
    x = np.max(np.stack([v[:n] for v in xs]), axis=0)
    attacks = np.sort(np.asarray(attacks, float))
    w = int(.04 * sr)
    lv = np.array([20 * np.log10(x[int(a * sr):int(a * sr) + w].max(initial=0.0) + 1e-9) for a in attacks])
    lv -= np.median(lv)
    out = [a for i, a in enumerate(attacks) if i and lv[i] < -QUIET_DB and REFRACTORY_S < attacks[i] - attacks[i - 1] <= ECHO_S
           and lv[i - 1] - lv[i] > LOUDER_DB]
    print(f'{track}: {len(out)} of {len(attacks)} stem attacks doubtful (quiet, just after a louder one)', flush=True)
    np.save(path, np.asarray(out, float))
    return np.load(path)


def confirmed(track, attacks):
    """The stem attacks (stem time, s) that a project kick note lands on; cached per track."""
    path = GOAL / f'projkicks_{track}.npy'
    if not path.exists():
        notes = note_times(extract(PROJECT[track]['als']), PROJECT[track]['kick'])
        attacks = np.sort(np.asarray(attacks, float))
        off = export_offset(attacks, notes)
        keep = attacks[nearest(attacks, notes + off) <= CONFIRM_MS / 1000]
        print(f'{track}: {len(keep)} of {len(attacks)} stem attacks on a kick note (offset {off:+.3f} s, '
              f'{len(notes)} notes)', flush=True)
        np.save(path, keep)
    return np.load(path)
