#!/usr/bin/env python3
"""Bass-hit feasibility survey (Peter, 2026-10-10: only prominent bass hits, as in Noisia/Skrillex drops, are events).

Songs: every song with a project and a proven offset (kick goal set via _source, plus the 2024/2025 WIPs). Per song:
bass tracks (name matches BASS: MIDI notes, and audio clips on those tracks), tempo, note count, and how many note/clip
starts are prominent in the mix: a 60-2000 Hz rise >= RISE_DB (peak in [-10, +40] ms over the median of [-90, -30] ms)
that is not within 30 ms of a kick label. Read-only projects (als_extract). Prints one line per song and a total."""
import os
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

BASS = re.compile(r'bass|sub\b|growl|reese|wobble|neuro|808|\bmid ?bass|donk|tear|screech|yoy|riddim', re.I)
NOT_BASS = re.compile(r'kick|drum|vocal|vox|bus|send|return|resolume', re.I)
RISE_DB = 6.0


def main():
    from tools.audio_analysis.eval.snare_labels import env_db
    from tools.audio_analysis.eval.snare_net import nearest
    from tools.audio_analysis.eval.detector_songs import audio, rate, wip
    from tools.audio_analysis.eval.detector_wip import kick_beats
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    g = Goal(mode='v3')
    W = wip()
    songs = [t for t in g.records if _source(t)] + [t for t in W if t != 'openings']
    tot_n = tot_p = 0
    for t in songs:
        als, off = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
        res = extract(als)
        bpm = res['tempo_points'][0]['bpm']
        starts, names = [], []
        for tr in res['tracks']:
            if tr.get('kind') == 'MidiTrack' and tr.get('speaker_on', True) and BASS.search(tr['name']) and not NOT_BASS.search(tr['name']):
                s = [n['s'] for c in tr['midi_clips'] for n in c['notes']]
                if s:
                    starts += s
                    names.append(f"{tr['name']}:{len(s)}")
        for c in res['audio_clips']:
            if BASS.search(c.get('track') or '') and not NOT_BASS.search(c.get('track') or ''):
                starts.append(c['start_s'])
        if not starts:
            print(f'{t:18s} {bpm:5.0f} bpm  no bass tracks', flush=True)
            continue
        x, sr = audio(g, t), rate(g, t)
        e = env_db(x, sr, 60, 2000)
        st = np.unique(np.round(np.array(starts) + off, 3))
        st = st[(st > .2) & (st < len(x) / sr - .2)]
        r = g.records.get(t, {})
        kicks = np.asarray(r.get('stem_kicks') if r.get('stem_kicks') is not None else r.get('all_labels', r.get('truth', [])), float)
        if t in W:
            kicks = kick_beats(res) * 60 / bpm + off
        rise = np.array([e[int(s * 1000) - 10:int(s * 1000) + 40].max() - np.median(e[int(s * 1000) - 90:int(s * 1000) - 30]) for s in st])
        prom = (rise >= RISE_DB) & (nearest(st, kicks) > .03)
        tot_n, tot_p = tot_n + len(st), tot_p + int(prom.sum())
        print(f'{t:18s} {bpm:5.0f} bpm  {len(st):5d} bass starts, {int(prom.sum()):4d} prominent off-kick | ' + ', '.join(names)[:120], flush=True)
    print(f'total {tot_n} bass starts, {tot_p} prominent off-kick in {len(songs)} songs')


if __name__ == '__main__':
    main()
