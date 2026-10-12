#!/usr/bin/env python3
"""Catalogue tests: role and version parsing, manual columns surviving a rebuild, online-only files never opened.
Run directly: test_catalog.py"""
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.catalog import catalog  # noqa: E402


class ParsingTests(unittest.TestCase):
    def test_roles(self):
        cases = [
            ('Late Night - Drums Stem.wav', ['STEMS'], 'drums'),
            ('Late Night - Kicks Stem.wav', [], 'kick'),
            ('Late Night - Bass and Sub Stem.wav', [], 'bass'),
            ('Late Night - FX Stem.wav', ['LATE NIGHT STEMS'], 'other'),
            ('LATE NIGHT - NO VOX.wav', ['LATE NIGHT STEMS'], 'full_mix'),
            ('Midnight Patience - Second Verse No Pianos.wav', ['STEMS'], 'full_mix'),
            ('Corrosion - PREMASTER - NO DRUMS.wav', ['PREMASTERS T2'], 'nodrums'),
            ('Corrosion - PREMASTER - DRUMS.wav', ['PREMASTERS T2'], 'drums'),
            ('Basalt WIP 4 - NO CHORDS NO DRUMS.wav', [], 'nodrums'),
            ('Pattern - NO KICK - 32Bit.wav', ['MASTERS', '32Bit'], 'nokick'),
            ('Pattern - Master (Premaster Fix) - V1.wav', ['MASTERS'], 'master'),
            ('Pattern - PREMASTER - 32Bit.wav', ['MASTERS'], 'premaster'),
            ('Pattern - NO LEADS - Master (Premaster Fix).wav', ['PATTERN STEMS'], 'full_mix'),
            ('Bettys - WIP 4 (Midnight Patience).mp3', [], 'full_mix'),
            ('Latent Space - 04 Midnight Patience T2 (mastered) 48k 24-bit.wav', ['ALEX T2'], 'master'),
            ('Drums snare 1.wav', ['STEMS'], 'snare'),
            ('_ HATS.wav', [], 'drums'),
            ('Know You Are There.wav', [], 'full_mix'),
            ('EatSlp_ Lead Perc.wav', [], 'drums'),
            ('Worship Stems - Sub 2-24b.wav', [], 'bass'),
        ]
        for name, parents, want in cases:
            self.assertEqual(catalog.classify(name, parents)[0], want, name)

    def test_fixture_roles(self):
        for name, want in (('mix.wav', 'full_mix'), ('drums.wav', 'drums'), ('others.wav', 'other'), ('vocals.wav', 'other'), ('bass.wav', 'bass')):
            self.assertEqual(catalog.classify(name, ['fixture:audio'], True)[0], want)

    def test_versions(self):
        cases = [
            ('Bettys - WIP 4 (Midnight Patience).mp3', [], 'WIP 4'),
            ('Latent Space - 04 Midnight Patience T2 (mastered) 48k 24-bit.wav', [], 'T2 MASTER'),
            ('01 - Right Where I Need You - PREMASTER - DRUMS.wav', [], 'PREMASTER'),
            ('Heavy on Mind MSTR V3 APRS.wav', [], 'MSTR V3'),
            ('01 - All In For You - ALBUM FINAL 1 - PREMASTER.wav', [], 'PREMASTER FINAL'),
            ('Drums.wav', ['PRE MASTERS T2'], 'PREMASTER T2'),
            ('Drums.wav', ['STEMS'], None),
            ('Know You_re There T1 (mastered).wav', [], 'T1 MASTER'),
        ]
        for name, parents, want in cases:
            self.assertEqual(catalog.parse_version(name, parents), want, name)


class DbTests(unittest.TestCase):
    def test_schema_version_and_migration(self):
        with tempfile.TemporaryDirectory() as d:
            con = catalog.connect(Path(d) / 'c.sqlite')
            self.assertEqual(con.execute('SELECT MAX(version) FROM schema_version').fetchone()[0], len(catalog.MIGRATIONS))
            con.close()
            con = catalog.connect(Path(d) / 'c.sqlite')      # reopening runs no migration twice
            self.assertEqual(con.execute('SELECT COUNT(*) FROM schema_version').fetchone()[0], len(catalog.MIGRATIONS))

    def test_rebuild_keeps_manual_columns(self):
        with tempfile.TemporaryDirectory() as d:
            con = catalog.connect(Path(d) / 'c.sqlite')
            row = dict(id='lh', title='LH', kind='project', licence='unknown', group_id='lh')
            catalog.upsert(con, 'songs', ('id',), row)
            con.execute("UPDATE songs SET notes='Peter says hi', group_override='other_group', licence_override='cc0' WHERE id='lh'")
            catalog.upsert(con, 'groups', ('id',), dict(id='lh', kind='single', size=1))
            con.execute("UPDATE groups SET note='keep' WHERE id='lh'")
            catalog.upsert(con, 'songs', ('id',), {**row, 'title': 'LH (rebuilt)', 'licence': 'own', 'group_id': 'x'})
            catalog.upsert(con, 'groups', ('id',), dict(id='lh', kind='versions', size=2))
            got = con.execute('SELECT title, licence, group_id, notes, group_override, licence_override FROM songs').fetchone()
            self.assertEqual(got, ('LH (rebuilt)', 'own', 'x', 'Peter says hi', 'other_group', 'cc0'))
            self.assertEqual(con.execute('SELECT size, note FROM groups').fetchone(), (2, 'keep'))
            eff = con.execute('SELECT grp, lic FROM song_groups').fetchone()
            self.assertEqual(eff, ('other_group', 'cc0'))

    def test_seed_fills_only_empty_manual_columns(self):
        with tempfile.TemporaryDirectory() as d:
            con = catalog.connect(Path(d) / 'c.sqlite')
            for t in ('worship', 'lh'):
                catalog.upsert(con, 'songs', ('id',), dict(id=t, title=t))
            con.execute("UPDATE songs SET licence_override='cc_by', notes='mine' WHERE id='lh'")
            catalog.seed_manual(con, {t: dict(licence_override='own_remix_private', notes='seed') for t in ('worship', 'lh')})
            got = dict((r[0], r[1:]) for r in con.execute('SELECT id, licence_override, notes FROM songs'))
            self.assertEqual(got['worship'], ('own_remix_private', 'seed'))
            self.assertEqual(got['lh'], ('cc_by', 'mine'))


class DatalessTests(unittest.TestCase):
    def test_online_only_file_is_never_opened(self):
        class Opened(Exception):
            pass

        def boom(*a, **k):
            raise Opened(a)
        import soundfile
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'online-only.wav'
            p.write_bytes(b'x' * 123)
            with mock.patch.object(catalog, 'is_dataless', return_value=True), mock.patch('builtins.open', boom), \
                    mock.patch.object(catalog, 'sha256_file', boom), mock.patch.object(catalog, 'loudness_peak', boom), \
                    mock.patch.object(soundfile, 'info', boom), mock.patch.object(soundfile, 'SoundFile', boom):
                meta = catalog.file_meta(str(p))
        self.assertEqual(meta['local'], 0)
        self.assertIsNone(meta['sha256'])
        self.assertEqual(meta['size'], 123)

    def test_scan_does_not_enter_online_only_folders(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / 'a').mkdir()
            (Path(d) / 'a' / 'x.wav').write_bytes(b'')
            (Path(d) / 'Samples').mkdir()
            (Path(d) / 'Samples' / 'y.wav').write_bytes(b'')
            (Path(d) / 'FORGE_SAMPLES #1').mkdir()
            (Path(d) / 'FORGE_SAMPLES #1' / 'z.wav').write_bytes(b'')
            found = [Path(p).name for p, _, _ in catalog.scan_audio(d, 2)]
            self.assertEqual(found, ['x.wav'])


class LoudnessTests(unittest.TestCase):
    def test_full_scale_sine_loudness(self):
        import numpy as np
        import soundfile as sf
        with tempfile.TemporaryDirectory() as d:
            t = np.arange(48000 * 6) / 48000
            x = np.sin(2 * np.pi * 997 * t)
            p = Path(d) / 's.wav'
            sf.write(str(p), np.stack([x, x], axis=1), 48000, subtype='FLOAT')
            lufs, pk = catalog.loudness_peak(str(p))
            self.assertAlmostEqual(lufs, 0.0, delta=.05)         # BS.1770: 997 Hz full-scale sine in both channels
            sf.write(str(p), x, 48000, subtype="FLOAT")
            self.assertAlmostEqual(catalog.loudness_peak(str(p))[0], -3.01, delta=.05)   # one channel
            self.assertAlmostEqual(pk, 0.0, delta=.01)


if __name__ == '__main__':
    unittest.main()
