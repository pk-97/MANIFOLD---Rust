#!/usr/bin/env python3
"""Snare + clap labels for the snare detector (one class: the backbeat snare/clap; rimshots and
cross-sticks do not — Peter, 2026-10-10; snare and kick rolls are unscored and untrained — Peter, 2026-10-11).
Writes GOAL/snare_labels.json through detector_labels (the label rules).

Snare specifics: snare/clap tracks and pads; rimshots and cross-sticks are vouched non-snares, and so are kicks, hats, rides
and percussion (Peter, 2026-10-10); prominence is judged on the mix's 1-8 kHz band (Peter: as loud as the song's main
snares, like a drum and bass snare); Lowkey is recall-only (its WIP offset is the survey's, not proven against a stem).
"""
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.detector_labels import Family  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import ABL, DROP, STEMS  # noqa: E402

SNARE = re.compile(r'snare|snr|clap|clp', re.I)
NOT_SNARE = re.compile(r'rim|xstick|x-stick|cross', re.I)
FAMILY = Family(
    name='snare',
    voice=SNARE,
    exclude=NOT_SNARE,
    other_voice=re.compile(r'kick|kik\b|\bbd\b|bass ?drum|hat|\bhh|ride|cymbal|crash|perc|shaker|shkr|tamb|tom|conga|bongo|cowbell|clave|wood|block|triangle|guiro|cabasa', re.I),
    other_parts=re.compile(r'drum|break|_top|top_?loop|perc|groove|fill|hat|ride|cymbal|shaker|tamb', re.I),
    not_other_parts=re.compile(r'kick|vocal|vox|synth|bass|string|pad|chord', re.I),
    band=(1000, 8000),
    drop_kick_rolls=True,
    midi_songs=('flight', '48_hours', 'pattern', 'back_to_you', 'lowkey', 'corrosion', 'burn_stems', 'midnight_patience',
                'heavy_on_mind', 'miracle', 'right_where', 'hung_up', 'got_it', 'renew', 'overwhelming_force'),
    # Project songs whose snares live (also) in audio clips on snare/clap tracks: one-shots, freezes, consolidations.
    clip_songs=('murmur', 'default_haze', 'late_night'),
    recall_only=('lowkey',),
    # Every-drum stems (local files): the whole song is known except their unexplained loud moments.
    drum_stems=(
        ('late_night', STEMS / 'LATE NIGHT STEMS/Late Night - Drums Stem.wav'),
        ('midnight_patience', STEMS / 'MIDNIGHT PATIENCE STEMS/Drums.wav'),
        ('back_to_you', ABL / '2024/States Project/STEMS/PRE MASTER/ALL DRUMS.wav'),
        ('burn_stems', ABL / '2025/Fifty Project/STEMS 3-08-25/DRUMS.wav'),
        ('pattern', ABL / '2025/Pattern Project/PATTERN STEMS/PATTERN - DRUMS.wav'),
        ('corrosion', ABL / '2026/Katsu Don Project/FINALS/PREMASTERS T2/Corrosion - PREMASTER - DRUMS.wav'),
        ('murmur', DROP / 'Interim - LS X EE - EP/PREMASTERS T2/04 - Murmur - PREMASTER - DRUMS.wav'),
        ('weight', STEMS / 'WEIGHT STEMS/Weight - Drum Stem.wav'),
        ('staged', ABL / '2024/Staged Project/WIP 2 Stems/Staged - Drums.wav'),
        ('basalt', ABL / '2025/Basalt Project/STEMS/Basalt WIP 4 - DRUMS.wav'),
        ('heavy_on_mind', STEMS / 'HEAVY ON MIND STEMS/DRUMS.wav'),
        ('miracle', STEMS / 'MIRACLE STEMS/Drums.wav'),
        ('right_where', DROP / 'Interim - LS X EE - EP/PREMASTERS T2/01 - Right Where I Need You - PREMASTER - DRUMS.wav'),
        ('hung_up', DROP / 'Interim - LS X EE - EP/PREMASTERS T2/03 - Hung Up - PREMASTER - DRUMS.wav'),
    ),
    # Goal songs whose mix is the sum of their stems: the snare and clap stems are the truth. Stems that may hold a
    # snare (snaps, beatbox, fills, percussion, drum loops, overhead and room mics) stay unknown where loud.
    multitracks=(
        ('cold_remix', re.compile(r'perc', re.I)),
        ('worship', re.compile(r'snap|beatbox|fill|perc', re.I)),
        ('business', re.compile(r'perc', re.I)),
        ('eat_sleep', re.compile(r'drums top|perc|conga', re.I)),
        ('lh', re.compile(r'^drums$', re.I)),
        ('gerrit', re.compile(r'overhead|room', re.I)),
    ),
)


def main():
    from tools.audio_analysis.eval.detector_labels import write
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    write(FAMILY, GOAL / 'snare_labels.json', lambda s: print(s, flush=True))


if __name__ == '__main__':
    main()
