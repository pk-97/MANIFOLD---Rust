#!/usr/bin/env python3
"""Tests for the whole-tree catalogue: Ableton set parsing on a small synthetic gzipped set, name -> composition rules,
folder skipping, variant verdict thresholds and an end-to-end variant comparison on synthetic audio.
No music folder needed. Run directly: test_tree.py"""
import gzip
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

from tools.audio_analysis.catalog import als_scan, comps, names, tree, usable, variants  # noqa: E402


def midi_clip(notes_by_key, start=0, end=64, disabled=False):
    kts = ''.join(
        f'<KeyTrack><Notes>' + ''.join(f'<MidiNoteEvent Time="{t}" Duration="0.25" Velocity="100" IsEnabled="true"/>' for t in ts) +
        f'</Notes><MidiKey Value="{k}"/></KeyTrack>' for k, ts in notes_by_key.items())
    return (f'<MidiClip Id="1"><CurrentStart Value="{start}"/><CurrentEnd Value="{end}"/><Disabled Value="{str(disabled).lower()}"/>'
            f'<Loop><LoopStart Value="0"/><LoopEnd Value="{end - start}"/><StartRelative Value="0"/><LoopOn Value="false"/></Loop>'
            f'<Notes><KeyTracks>{kts}</KeyTracks></Notes></MidiClip>')


def track(kind, tid, name, clips='', group=-1, speaker=True, devices='', audio=False):
    seq = ('<Sample><ArrangerAutomation><Events>' + clips + '</Events></ArrangerAutomation></Sample>' if audio else
           '<ClipTimeable><ArrangerAutomation><Events>' + clips + '</Events></ArrangerAutomation></ClipTimeable>')
    return (f'<{kind} Id="{tid}"><Name><EffectiveName Value="{name}"/></Name><TrackGroupId Value="{group}"/>'
            f'<DeviceChain><Mixer><Speaker><Manual Value="{str(speaker).lower()}"/></Speaker></Mixer>'
            f'<DeviceChain><Devices>{devices}</Devices></DeviceChain><MainSequencer>{seq}</MainSequencer></DeviceChain></{kind}>')


def audio_clip(path, end=32):
    return (f'<AudioClip Id="2"><CurrentStart Value="0"/><CurrentEnd Value="{end}"/><Disabled Value="false"/>'
            f'<Loop><LoopStart Value="0"/><LoopEnd Value="{end}"/><StartRelative Value="0"/><LoopOn Value="false"/></Loop>'
            f'<IsWarped Value="false"/><SampleRef><FileRef><Path Value="{path}"/></FileRef></SampleRef></AudioClip>')


RACK = ('<DrumGroupDevice><Branches>'
        '<DrumBranch><Name><EffectiveName Value="Snare 7"/></Name><BranchInfo><ReceivingNote Value="90"/></BranchInfo>'
        '<MixerDevice><Speaker><Manual Value="true"/></Speaker></MixerDevice><DeviceChain><Instrument><Devices/></Instrument></DeviceChain></DrumBranch>'
        '<DrumBranch><Name><EffectiveName Value="Closed Hat"/></Name><BranchInfo><ReceivingNote Value="89"/></BranchInfo>'
        '<MixerDevice><Speaker><Manual Value="true"/></Speaker></MixerDevice><DeviceChain><Instrument><Devices/></Instrument></DeviceChain></DrumBranch>'
        '</Branches></DrumGroupDevice>')


def write_set(path, tracks, bpm=128):
    xml = ('<?xml version="1.0" encoding="UTF-8"?><Ableton Creator="Ableton Live 12.0"><LiveSet><Tracks>' + ''.join(tracks) +
           f'</Tracks><MainTrack><DeviceChain><Mixer><Tempo><Manual Value="{bpm}"/><AutomationTarget Id="9"/></Tempo></Mixer>'
           f'</DeviceChain></MainTrack><Scenes/></LiveSet></Ableton>')
    with gzip.open(path, 'wb') as f:
        f.write(xml.encode())


class SetParseTests(unittest.TestCase):
    def scan(self, tracks):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'song.als'
            write_set(p, tracks)
            return als_scan.scan(p)

    def test_kick_track_and_drum_rack_pads(self):
        kicks = list(range(0, 64, 2))                       # 32 kick notes
        r = self.scan([
            track('MidiTrack', 1, '40-KICK', midi_clip({36: kicks})),
            track('MidiTrack', 2, '44-QFF Drums', midi_clip({38: [1, 3, 5, 7] * 8, 39: [0.5 * i for i in range(40)]}), devices=RACK),
        ])
        rd = r['ready']
        self.assertEqual(r['tempo'], 128.0)
        self.assertEqual(rd['verdict'], 'ready')
        self.assertEqual(rd['kick_notes'], 32)
        self.assertEqual(rd['snare_notes'], 32)           # pad 38 is "Snare 7": found by pad name
        self.assertEqual(rd['hat_notes'], 40)             # pad 39 is "Closed Hat"
        self.assertIn('40-KICK (32)', rd['kick_tracks'])
        self.assertEqual(r['arrangement_beats'], 64.0)
        self.assertAlmostEqual(r['arrangement_s'], 64 * 60 / 128, places=1)

    def test_muted_group_makes_notes_unknown_not_silent(self):
        r = self.scan([
            track('GroupTrack', 10, 'DRUMS GROUP', speaker=False),
            track('MidiTrack', 1, 'Kick', midi_clip({36: list(range(0, 64))}), group=10),
        ])
        self.assertEqual(r['ready']['verdict'], 'muted_only')
        self.assertEqual(r['ready']['kick_notes'], 0)
        self.assertEqual(r['ready']['muted_kick'], 64)

    def test_audio_clip_drums_are_flagged_not_midi(self):
        r = self.scan([track('AudioTrack', 1, 'Snare Clap', audio_clip('/x/snare.wav'), audio=True),
                       track('MidiTrack', 2, '19-Serum', midi_clip({60: list(range(0, 64))}))])
        self.assertEqual(r['ready']['verdict'], 'audio_only')
        self.assertEqual(r['ready']['audio_drums'][0]['track'], 'Snare Clap')
        self.assertEqual(r['ready']['audio_drums'][0]['voice'], 'snare')

    def test_melodic_midi_and_few_notes_are_not_label_ready(self):
        r = self.scan([track('MidiTrack', 1, '23-Basic Analog Bass', midi_clip({40: list(range(0, 64))})),
                       track('MidiTrack', 2, 'Kick', midi_clip({36: [0, 4, 8]}))])
        self.assertEqual(r['ready']['verdict'], 'none')
        self.assertEqual(r['ready']['kick_notes'], 3)

    def test_disabled_clip_plays_nothing(self):
        r = self.scan([track('MidiTrack', 1, 'Kick', midi_clip({36: list(range(0, 64))}, disabled=True))])
        self.assertEqual(r['ready']['kick_notes'], 0)

    def test_voice_names(self):
        for name, want in (('40-KICK', 'kick'), ('DS Kick', 'kick'), ('Snare 7', 'snare'), ('Clap Room', 'snare'),
                           ('Rim Shot', 'perc'), ('Closed Hat', 'hat'), ('Ride', 'hat'), ('Conga', 'perc'), ('Serum', 'other')):
            self.assertEqual(als_scan.voice_of(name), want, name)


class NameTests(unittest.TestCase):
    def test_titles(self):
        cases = [
            ('Latent Space - 04 Midnight Patience T2 (mastered) 48k 24-bit.wav', 'Midnight Patience', []),
            ('Imitation (Pusher) - PREMASTER T2.als', 'Imitation', ['Pusher']),
            ('Kx5 - Eat Sleep (LATENT SPACE Remix) [PREMASTER].wav', 'Eat Sleep', []),
            ('48 Hours - WIP 2.mp3', '48 Hours', []),
            ('02 - Heavy on Mind MSTR V3 APRS.wav', 'Heavy on Mind', []),
            ('Steady (Right Where I Need You) - FINAL 03.wav', 'Steady', ['Right Where I Need You']),
            ('Know You_re There x ZOIIA - WIP 3.mp3', "Know You're There", []),
            ('Marshmello, Halsey - Be Kind (Original Mix).mp3', 'Be Kind', []),
            ('Growth V3 T2 PRE MASTER (All In For You - Tayla Vox).als', 'Growth', ['All In For You']),
            ('03 - Hung Up - PREMASTER - DRUMS.wav', 'Hung Up', []),
            ('cool drums 12.2.als', 'cool drums', []),
        ]
        for text, title, aliases in cases:
            self.assertEqual(names.main_title(text), (title, aliases), text)

    def test_generic_stem_names_defer_to_the_folder(self):
        for t in ('DRUMS', 'Bass and Sub', 'NO DRUMS', 'ALL DRUMS', 'KICK'):
            self.assertTrue(names.generic(t), t)
        self.assertFalse(names.generic('Weight'))
        self.assertEqual(names.folder_title(['Ableton Projects', 'STEMS', 'LATE NIGHT STEMS']), 'LATE NIGHT')
        self.assertEqual(names.folder_title(['Ableton Projects', '2025', 'Rainfall Project', 'STEMS']), 'Rainfall')


class TreeTests(unittest.TestCase):
    def test_skip_rules(self):
        for name in ('Samples', 'Backup', 'Ableton Project Info', 'FORGE_SAMPLES #1', 'Moss - GROUNDED Mini Pack', 'Imported',
                     'Presets', 'Folder Info'):
            self.assertTrue(tree.SKIP_DIR.search(name), name)
        for name in ('Other And Backups', 'Blender Sample Test Project', 'STEMS', 'FINALS', 'PREMASTERS T2', 'Remix Stems'):
            self.assertFalse(tree.SKIP_DIR.search(name), name)

    def test_walk_reads_stat_only_and_skips(self):
        with tempfile.TemporaryDirectory() as d:
            m = Path(d)
            (m / 'Ableton Projects/2025/A Project/Samples').mkdir(parents=True)
            (m / 'Ableton Projects/2025/A Project/FINALS').mkdir()
            (m / 'Ableton Projects/2025/A Project/Samples/x.wav').write_bytes(b'')
            (m / 'Ableton Projects/2025/A Project/FINALS/A - FINAL 01.wav').write_bytes(b'')
            (m / 'Ableton Projects/2025/A Project/A.als').write_bytes(b'')
            (m / 'Samples').mkdir()
            (m / 'Samples/y.wav').write_bytes(b'')
            audio, sets, skipped = tree.walk(m)
            self.assertEqual([Path(p).name for p, _ in audio], ['A - FINAL 01.wav'])
            self.assertEqual([Path(p).name for p in sets], ['A.als'])
            self.assertEqual(sorted(Path(p).name for p, *_ in skipped), ['Samples', 'Samples'])
            self.assertEqual(tree.project_dir(str(m / 'Ableton Projects/2025/A Project/FINALS/a.wav'), m),
                             str(m / 'Ableton Projects/2025/A Project'))


class CompTests(unittest.TestCase):
    def setUp(self):
        self.res = comps.Resolver({'midnightpatience': 'midnight_patience', 'bettys': 'midnight_patience'})

    def test_file_rules(self):
        known = {'midnightpatience', 'bettys'}
        cases = [
            ('Ableton Projects/2024/Bettys Project/Bettys - WIP 4 (Midnight Patience).mp3', 'Bettys', 'midnight_patience'),
            ('Ableton Projects/STEMS/MIDNIGHT PATIENCE STEMS/Drums.wav', None, 'midnight_patience'),
            ('Ableton Projects/2025/Zephyr Project/STEMS/KICK.wav', 'Zephyr', 'zephyr'),
            ('EMERGENCE - ALBUM 2026/FINAL MASTERS/Latent Space - 04 - Midnight Patience T2 (mastered) 48k 24-bit.wav', None,
             'midnight_patience'),
            ('Ableton Projects/2024/Texture Project/TAYLA VOX/Adlibs.wav', 'Texture', 'heavy_on_mind'),
            ('Ableton Projects/Ableton 11/Growth Project/Growth - WIP 1.mp3', 'Growth', 'growth_2023'),
        ]
        for rel, project, want in cases:
            got = comps.comp_of_file(self.res, rel, known, project)[0]
            self.assertEqual(got, want, rel)

    def test_pack_folders_are_one_song_per_artist(self):
        r = comps.comp_of_file(self.res, comps.PACK_ROOT + '/DIY Super Group Pack 2022/PUP/STEM-Satellites- Bass.wav', set(), None)
        self.assertEqual(r[0], 'pup')
        r = comps.comp_of_file(self.res, comps.PACK_ROOT + '/Koven - Gold Stems/Bass.wav', set(), None)
        self.assertEqual(r[0], 'koven')

    def test_union_find(self):
        u = comps.Union()
        u.join('a', 'b')
        u.join('c', 'b')
        self.assertEqual(u.find('a'), u.find('c'))
        self.assertNotEqual(u.find('a'), u.find('z'))


class VerdictTests(unittest.TestCase):
    base = dict(lag_z=30.0, seg_frac=1.0, drift_ms=0.0, cover=1.0)

    def v(self, **kw):
        return variants.verdict({**self.base, **kw})

    def test_thresholds(self):
        self.assertEqual(self.v(), 'same_arrangement')
        self.assertEqual(self.v(lag_z=variants.Z_MIN - .1), 'different_arrangement')
        self.assertEqual(self.v(seg_frac=variants.SEG_DIFF - .01), 'different_arrangement')
        self.assertEqual(self.v(drift_ms=variants.DRIFT_DIFF_MS + 1), 'different_arrangement')
        self.assertEqual(self.v(seg_frac=variants.SEG_SAME - .01), 'aligned_needs_check')
        self.assertEqual(self.v(seg_frac=variants.SEG_SAME), 'same_arrangement')
        self.assertEqual(self.v(drift_ms=variants.DRIFT_SAME_MS + 1), 'aligned_needs_check')
        self.assertEqual(self.v(cover=variants.COVER_SAME - .01), 'aligned_needs_check')


class DistinctTests(unittest.TestCase):
    def test_identical_copies_are_one_variant(self):
        c = [('a.wav', 'master', 'T1', 'S1', 100.0), ('b.wav', 'master', 'T1', 'S1', 100.0), ('c.wav', 'master', 'T2', 'S2', 100.0),
             ('d.wav', 'full_mix', '', 'LAB', 100.0)]
        self.assertEqual([x[0] for x in variants.distinct(c, {'LAB'})], ['a.wav', 'c.wav'])


SR = 24000


def song(seed=0, secs=64, edit_after=None, pad=False):
    """Two-second bars of kick, snare and hat hits on a sixteen-slot grid (positions drawn from the seed), under a
    bass line. edit_after redraws the patterns after that second: the same song, a different second half. pad lays a
    loud detuned saw pad with a noise floor over everything: the wideband level is then the pad's, as in a full mix."""
    n = SR * secs
    t = np.arange(n) / SR
    x = np.zeros(n)
    noise = np.random.default_rng(seed + 1000)
    kick = np.sin(2 * np.pi * 55 * np.arange(int(.15 * SR)) / SR) * np.exp(-np.arange(int(.15 * SR)) / (.04 * SR))
    snare = noise.standard_normal(int(.12 * SR)) * np.exp(-np.arange(int(.12 * SR)) / (.03 * SR))
    hat = noise.standard_normal(int(.02 * SR)) * np.exp(-np.arange(int(.02 * SR)) / (.005 * SR))

    def hit(at, sig):
        a = int(at * SR)
        if 0 <= a < n:
            m = min(len(sig), n - a)
            x[a:a + m] += sig[:m]

    def pattern(s):
        r = np.random.default_rng(s)
        return (sorted(r.choice(16, 3, replace=False)), sorted(r.choice(16, 2, replace=False)), sorted(r.choice(16, 4, replace=False)))
    base, edited = pattern(seed), pattern(seed + 100)
    for bar in range(secs // 2):
        ks, ss, hs = edited if edit_after is not None and bar * 2 >= edit_after else base
        for q in ks:
            hit(bar * 2 + q * .125, kick * .9)
        for q in ss:
            hit(bar * 2 + q * .125, snare * .6)
        for q in hs:
            hit(bar * 2 + q * .125, hat * .5)
    notes = [110, 146.8, 164.8, 196]
    r = np.random.default_rng(seed + 5)
    for i in range(secs // 4):
        seg = (t >= i * 4) & (t < i * 4 + 4)
        x[seg] += .25 * np.sin(2 * np.pi * notes[int(r.integers(4))] * t[seg])
    if pad:
        root = 220.0 * 2 ** (int(np.random.default_rng(seed + 9).integers(12)) / 12)
        for f in (root, root * 1.26, root * 1.5, root * 2):
            for detune in (.994, 1.0, 1.006):
                x += .12 * (2 * ((t * f * detune) % 1.0) - 1)
        x += .02 * np.random.default_rng(seed + 7).standard_normal(n)
    return x


def wash(n, level=.6, seed=3):
    """Broadband noise with a wandering 50 ms level: what a final carries that the lab mix's stem sum lacks (reverb
    returns, vocal breath, bus noise). Its level jitter swamps a wideband onset envelope."""
    r = np.random.default_rng(seed)
    env = np.abs(np.convolve(r.standard_normal(n), np.ones(int(.05 * SR)) / int(.05 * SR), 'same'))
    return level * r.standard_normal(n) * env / env.mean()


def limited(x, ceiling=.35, release_s=.15):
    """A heavy lookahead limiter: instant attack over 3 ms of lookahead, one-pole release. Pumps the whole mix on every
    loud hit, as a mastering limiter does on a final; the lab mix (a stem sum) has none."""
    from scipy.ndimage import maximum_filter1d, minimum_filter1d
    from scipy.signal import lfilter
    hold = maximum_filter1d(np.abs(x), int(.003 * SR))
    reduction = 1.0 - np.minimum(1.0, ceiling / np.maximum(hold, 1e-9))
    reduction = maximum_filter1d(reduction, int(.003 * SR))
    a = np.exp(-1.0 / (release_s * SR))
    smoothed = lfilter([1 - a], [1, -a], reduction)
    smoothed = minimum_filter1d(smoothed, 1)
    return x * (1.0 - np.maximum(reduction, smoothed))


class ClassifyTests(unittest.TestCase):
    base = dict(kind='original', songs=[], labels={}, bounces_local=1, bounces_online=0, stems_local=0, stems_online=0, sets=1,
                ready_sets=0, audio_only_sets=0, muted_sets=0, unread_sets=0)

    def state(self, **kw):
        return usable.classify(dict(self.base, **kw))

    def test_ready_needs_local_audio(self):
        self.assertEqual(self.state(ready_sets=1)[0], 'label_ready')
        self.assertEqual(self.state(ready_sets=1, bounces_local=0, bounces_online=2)[0], 'label_ready_online')

    def test_blocked_reasons(self):
        self.assertIn('audio clips', self.state(audio_only_sets=1)[1])
        self.assertIn('muted', self.state(muted_sets=1)[1])
        self.assertIn('no Ableton set', self.state(sets=0)[1])
        self.assertIn('online-only (unread)', self.state(sets=3, unread_sets=1)[1])
        self.assertEqual(self.state(bounces_local=0)[1], 'no audio bounce or stems')

    def test_lab_and_stem_packs(self):
        self.assertEqual(self.state(songs=['a'], labels={'a': {'kick': 4}})[0], 'lab')
        self.assertEqual(self.state(kind='stem_pack')[0], 'blocked')


class CompareTests(unittest.TestCase):
    def test_same_song_mastered_and_shifted_is_same_arrangement(self):
        ref = song()
        from scipy.signal import butter, sosfilt
        mastered = np.tanh(2.2 * sosfilt(butter(2, 9000, fs=SR, output='sos'), ref)) * .8
        delay = int(.137 * SR)
        x = np.concatenate([np.zeros(delay), mastered])[:len(ref) + 2000]
        m = variants.compare(ref, x, SR)
        self.assertEqual(m['verdict'], 'same_arrangement', m)
        self.assertAlmostEqual(m['lag_s'], -.137, delta=.002)       # x is late by 137 ms: its events sit 137 ms earlier in ref

    def test_limited_final_on_a_shifted_timeline_is_same_arrangement(self):
        """The Back to You failure: finals sat at one constant lag from the lab mix yet only ~30% of windows matched,
        because a fixed onset correlation (0.5) is out of reach for a limited bounce of a full mix against an unlimited
        sum. Same timeline, mastered: the labels transfer."""
        ref = song(pad=True)
        final = limited(ref + wash(len(ref)))
        self.assertLess(float(np.abs(final).max()), .4)                  # the limiter really works
        delay = int(2.61 * SR)
        x = np.concatenate([np.zeros(delay), final])
        m = variants.compare(ref, x, SR)
        self.assertEqual(m['verdict'], 'same_arrangement', m)
        self.assertAlmostEqual(m['lag_s'], -2.61, delta=.002)

    def test_one_replaced_section_of_a_limited_shifted_final_is_flagged(self):
        """The same final with 16 s swapped for another piece of music is not the same arrangement: the lag still fits
        (one timeline) but the windows over the section do not."""
        ref = song(pad=True)
        edited = ref.copy()
        edited[24 * SR:40 * SR] = song(seed=5, pad=True)[24 * SR:40 * SR]
        x = np.concatenate([np.zeros(int(2.61 * SR)), limited(edited + wash(len(edited)))])
        m = variants.compare(ref, x, SR)
        self.assertIn(m['verdict'], ('aligned_needs_check', 'different_arrangement'), m)
        self.assertLess(m['seg_frac'], variants.SEG_SAME)
        self.assertGreater(m['seg_frac'], variants.SEG_DIFF)             # most of the song still lines up

    def test_changed_second_half_is_not_same(self):
        ref = song()
        edited = song(edit_after=32)
        m = variants.compare(ref, edited, SR)
        self.assertNotEqual(m['verdict'], 'same_arrangement', m)
        self.assertLess(m['seg_frac'], variants.SEG_SAME)

    def test_unrelated_song_is_different(self):
        m = variants.compare(song(0), song(5), SR)
        self.assertEqual(m['verdict'], 'different_arrangement', m)


if __name__ == '__main__':
    unittest.main()
