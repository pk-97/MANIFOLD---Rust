"""Peter's generated label sets (note_clean.PROJECT, read only), shared by every family's generated labels: MIDI only,
nothing muted, every note probability 1, one tempo per section, so every note is a certain label. Train only; the
held-out exam stays on real songs (Peter, 2026-10-11).
- V1 ("Labels and Timings.als"): one section per LinnDrum track (four kits, 133 of 416 rack settings differ).
- Rolls ("Labels and Timings Rolls.als"): one section per locator block: VAR 1-4 (groove, kick and snare build-up
  rolls, drops) and MUSIC 1-12 (genre sections, 90-174 BPM).
Times are export seconds: note seconds minus the first note's start."""
import re

import numpy as np

SR = 48000
SETS = (('Labels and Timings.als', 'V1 FULL MIX', 'kits'), ('Labels and Timings Rolls.als', 'Rolls FULL MIX', 'locators'))
KICK_KEYS = {36}


def sections(res, how, start):
    """[(name, a, b)] in export seconds."""
    if how == 'kits':
        out = []
        for k, tr in enumerate([t for t in res['tracks'] if 'LinnDrum' in t['name']], 1):
            out.append((f'linn_{k}', min(c['start_s'] for c in tr['midi_clips']) - start, max(c['end_s'] for c in tr['midi_clips']) - start))
        return out
    blocks = []
    for loc in res['locators']:
        m = re.match(r'(VAR|MUSIC) (\d+)', loc['name'])
        key = f'rolls_{m.group(1).lower()}{int(m.group(2)):02d}' if m else None
        if blocks and blocks[-1][0] != key:
            blocks[-1][2] = loc['s'] - start
        if key and (not blocks or blocks[-1][0] != key):
            blocks.append([key, loc['s'] - start, None])
    return [tuple(b) for b in blocks if b[2] is not None]


def export_path(export):
    from tools.audio_analysis.eval.note_clean import PROJECT
    return str(PROJECT / 'Exports' / f'Labels and Timings - {export}.wav')


def load(als, export):
    """(extract result, export start in project seconds, 48 kHz mono export) for one set."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_labels import read_any
    from tools.audio_analysis.eval.kick_goal_eval import load as load_audio
    from tools.audio_analysis.eval.note_clean import PROJECT
    res = extract(PROJECT / als)
    start = min(c['start_s'] for tr in res['tracks'] for c in tr['midi_clips'] if c['notes'])
    return res, start, read_any(export_path(export), SR, load_audio)


def kicks(res, start):
    """Kick-pad note times (export seconds) on the LinnDrum tracks."""
    return np.sort([n['s'] - start for tr in res['tracks'] if 'LinnDrum' in tr['name']
                    for c in tr['midi_clips'] for n in c['notes'] if n['key'] in KICK_KEYS])
