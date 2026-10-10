#!/usr/bin/env python3
"""Snare + clap labels for the snare detector (one class: the backbeat snare/clap; rolls count; rimshots and
cross-sticks do not — Peter, 2026-10-10). Writes GOAL/snare_labels.json through detector_labels (the label rules).

Snare specifics: snare/clap tracks and pads; rimshots and cross-sticks are vouched non-snares, and so are hats, rides
and percussion (Peter, 2026-10-10); prominence is judged on the mix's 1-8 kHz band (Peter: as loud as the song's main
snares, like a drum and bass snare); Lowkey is recall-only (its WIP offset is the survey's, not proven against a stem).
"""
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.detector_labels import Family  # noqa: E402

SNARE = re.compile(r'snare|snr|clap|clp', re.I)
NOT_SNARE = re.compile(r'rim|xstick|x-stick|cross', re.I)
FAMILY = Family(
    name='snare',
    voice=SNARE,
    exclude=NOT_SNARE,
    other_voice=re.compile(r'hat|\bhh|ride|cymbal|crash|perc|shaker|shkr|tamb|tom|conga|bongo|cowbell|clave|wood|block|triangle|guiro|cabasa', re.I),
    other_parts=re.compile(r'drum|break|_top|top_?loop|perc|groove|fill|hat|ride|cymbal|shaker|tamb', re.I),
    not_other_parts=re.compile(r'kick|vocal|vox|synth|bass|string|pad|chord', re.I),
    band=(1000, 8000),
    midi_songs=('flight', '48_hours', 'pattern', 'back_to_you', 'lowkey', 'corrosion', 'burn_stems', 'midnight_patience'),
    # Project songs whose snares live (also) in audio clips on snare/clap tracks: one-shots, freezes, consolidations.
    clip_songs=('murmur', 'default_haze', 'late_night'),
    stem_songs={'cold_remix': ('_ SNARE.wav',), 'gerrit': ('Drums snare 1.wav', 'Drums snare 2 (popcorn).wav'), 'business': ('@Claps.wav',),
                'worship': ('Worship Stems - MM_clap_smacc_dry-24b.wav', 'Worship Stems - MM_crisp_ass_trap_snare-24b.wav',
                            'Worship Stems - MM_jungleclap-24b.wav')},
    # Stems whose hits are vouched non-snares (finger snaps: Peter, 2026-10-10), and stems that may hold extra snares
    # (drum buses, fills, percussion) whose hits are left unscored.
    stem_neg={'worship': ('Worship Stems - KSHMR Snap 01-24b.wav', 'Worship Stems - KSHMR Snap 05-24b.wav', 'Worship Stems - MM_2099_snap_01-24b.wav')},
    stem_doubt={'worship': ('Worship Stems - Bus 1-24b.wav', 'Worship Stems - Bus 2-24b.wav', 'Worship Stems - KSHMR Acoustic Fill 128BPM 03-24b.wav',
                            'Worship Stems - MM_percussion_billow-24b.wav')},
    recall_only=('lowkey',),
)


def main():
    from tools.audio_analysis.eval.detector_labels import write
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    write(FAMILY, GOAL / 'snare_labels.json', lambda s: print(s, flush=True))


if __name__ == '__main__':
    main()
