"""Peter's generated label sets (note_clean.PROJECT, read only), shared by every family's generated labels: MIDI only,
every note probability 1, so every note is a certain label. Train only; the held-out exam stays on real songs
(Peter, 2026-10-11). Each set names its project, its full-mix export, where the export starts on the project timeline,
how it splits into sections and its Drums stem, which proves the timing (timing_proof).
- V1 ("Labels and Timings.als"): one section per LinnDrum track (four kits, 133 of 416 rack settings differ).
- Rolls ("Labels and Timings Rolls.als"): one section per locator block: VAR 1-4 (groove, kick and snare build-up
  rolls, drops) and MUSIC 1-12 (genre sections, 90-174 BPM).
- Modulations ("Labels and Timings - Modulations.als"): one section per locator part, 15 genres at 90-174 BPM; the
  export starts at bar 17. Its Leads and Bass groups are saved muted after the stem exports; their notes only ever
  serve as vouched negatives.
Drum pads are the LinnDrum tracks' (KICK_KEYS, the snare builder's keys); drum tracks must be audible in the project.
Times are export seconds."""
import re
from dataclasses import dataclass

import numpy as np

SR = 48000
KICK_KEYS = {36}
DRUM_TRACKS = re.compile('LinnDrum')
PROOF_MAX_MS, PROOF_MIN_HITS = 3.0, 100   # median attack lag of isolated drum hits, and how many must be measured


@dataclass(frozen=True)
class GenSet:
    als: str
    export: str            # full mix, under PROJECT/Exports
    drums: str             # Drums stem, under PROJECT/Exports
    sections: str          # 'kits' | 'locators' | 'parts'
    prefix: str            # section song ids: f'{prefix}_...'
    start_s: float | None  # export start on the project timeline; None: the first MIDI note

    @property
    def path(self):
        from tools.audio_analysis.eval.note_clean import PROJECT
        return str(PROJECT / 'Exports' / self.export)

    @property
    def drums_path(self):
        from tools.audio_analysis.eval.note_clean import PROJECT
        return str(PROJECT / 'Exports' / self.drums)


MODS = 'Claude Album 2026 - Modulations/Labels and Timings - Modulations'
SETS = (GenSet('Labels and Timings.als', 'Labels and Timings - V1 FULL MIX.wav', 'Labels and Timings - V1 DRUMS.wav',
               'kits', 'linn', None),
        GenSet('Labels and Timings Rolls.als', 'Labels and Timings - Rolls FULL MIX.wav',
               'Labels and Timings - Rolls DRUMS.wav', 'locators', 'rolls', None),
        # Bar 17 at 140 BPM (Peter, 2026-10-11: "Audio starts at bar 17 = 27.429 s").
        GenSet('Labels and Timings - Modulations.als', f'{MODS} - Full Mix.wav', f'{MODS} - Drums.wav', 'parts', 'mod',
               16 * 4 * 60 / 140))


def sections(res, gset, start):
    """[(song id, a, b)] in export seconds."""
    if gset.sections == 'kits':
        out = []
        for k, tr in enumerate([t for t in res['tracks'] if 'LinnDrum' in t['name']], 1):
            out.append((f'{gset.prefix}_{k}', min(c['start_s'] for c in tr['midi_clips']) - start,
                        max(c['end_s'] for c in tr['midi_clips']) - start))
        return out
    locs = sorted(res['locators'], key=lambda l: l['s'])
    if gset.sections == 'parts':
        # Every locator starts a part that runs to the next; the last locator ends the export.
        return [(f'{gset.prefix}_{k:02d}', a['s'] - start, b['s'] - start) for k, (a, b) in enumerate(zip(locs, locs[1:]), 1)]
    blocks = []
    for loc in locs:
        m = re.match(r'(VAR|MUSIC) (\d+)', loc['name'])
        key = f'{gset.prefix}_{m.group(1).lower()}{int(m.group(2)):02d}' if m else None
        if blocks and blocks[-1][0] != key:
            blocks[-1][2] = loc['s'] - start
        if key and (not blocks or blocks[-1][0] != key):
            blocks.append([key, loc['s'] - start, None])
    return [tuple(b) for b in blocks if b[2] is not None]


def audible(res, track):
    """Plays in the export: the track and every group above it are on."""
    on = {t['name']: t['speaker_on'] for t in res['tracks']}
    return track['speaker_on'] and all(on.get(g, True) for g in track['groups'])


def load(gset):
    """(extract result, export start in project seconds, 48 kHz mono full mix) for one set. Exits when a drum track is
    muted, when a stated start disagrees with the first note by over 1 ms, or when the export is shorter than the
    last section."""
    import sys

    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_labels import read_any
    from tools.audio_analysis.eval.kick_goal_eval import load as load_audio
    from tools.audio_analysis.eval.note_clean import PROJECT
    res = extract(PROJECT / gset.als)
    muted = [t['name'] for t in res['tracks'] if DRUM_TRACKS.search(t['name']) and not audible(res, t)]
    if muted:
        sys.exit(f'{gset.als}: drum tracks muted in the project: {muted}; their hits are unknown')
    first = min(c['start_s'] for tr in res['tracks'] for c in tr['midi_clips'] if c['notes'])
    start = first if gset.start_s is None else gset.start_s
    if abs(first - start) > .001 and min(n['s'] for tr in res['tracks'] for c in tr['midi_clips'] for n in c['notes']) < start:
        sys.exit(f'{gset.als}: notes before the stated export start {start:.4f} s')
    x = read_any(gset.path, SR, load_audio)
    end = max(b for _, _, b in sections(res, gset, start))
    if len(x) / SR < end - .05:
        sys.exit(f'{gset.export}: {len(x) / SR:.2f} s of audio for sections up to {end:.2f} s')
    return res, start, x


def drum_notes(res, start):
    """[(export s, pad key or None for a non-drum note)] over every MIDI note."""
    return [(n['s'] - start, n['key'] if DRUM_TRACKS.search(tr['name']) else None) for tr in res['tracks']
            for c in tr['midi_clips'] for n in c['notes']]


def kicks(res, start):
    """Kick-pad note times (export seconds) on the drum tracks."""
    return np.sort([t for t, key in drum_notes(res, start) if key in KICK_KEYS])


def timing_proof(gset, res, start):
    """The export's timing against its Drums stem: for drum hits with nothing in the 80 ms before them, the lag from
    the MIDI time to where the stem's envelope first clears the preceding level by 10 dB. A wrong start or a drifting
    export moves the median; kit shapes do not. Returns dict(median_ms, p10_ms, p90_ms, hits, proven)."""
    from tools.audio_analysis.eval.detector_labels import read_any
    from tools.audio_analysis.eval.kick_goal_eval import load as load_audio
    from tools.audio_analysis.eval.kick_goal_labels import kick_env_db
    e = kick_env_db(read_any(gset.drums_path, SR, load_audio), SR)
    hits = np.unique(np.round([t for t, key in drum_notes(res, start) if key is not None], 4))
    iso = hits[np.concatenate([[True], np.diff(hits) > .08])] if len(hits) else hits
    lags = []
    for k in iso:
        i = int(round(k * 1000))
        if i < 45 or i + 30 >= len(e):
            continue
        up = np.flatnonzero(e[i - 15:i + 25] >= np.median(e[i - 40:i - 15]) + 10)
        if len(up):
            lags.append(up[0] - 15)
    v = np.array(lags, float)
    med = float(np.median(v)) if len(v) else float('nan')
    return dict(median_ms=med, p10_ms=float(np.percentile(v, 10)) if len(v) else None,
                p90_ms=float(np.percentile(v, 90)) if len(v) else None, hits=int(len(v)),
                proven=bool(len(v) >= PROOF_MIN_HITS and abs(med) <= PROOF_MAX_MS))
