#!/usr/bin/env python3
"""Label review project tests (synthetic songs and a fake project_tool; no music folder, no app).
Run directly: test_review_project.py"""
import json
import math
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

from tools.audio_analysis.lab import review_project as rp  # noqa: E402

# A stand-in for project_tool's label-review verbs: the project is plain JSON, seconds come from the same step map.
FAKE_TOOL = r'''#!/usr/bin/env python3
import json, sys, uuid
sys.path.insert(0, ROOT)
from tools.audio_analysis.lab.review_project import StepMap
a = sys.argv[1:]
def flag(n):
    return a[a.index(n) + 1]
def load(p):
    return json.load(open(p))
def save(p, d):
    json.dump(d, open(p, 'w'))
verb, p = ' '.join(a[:2]), a[2]
if verb == 'project new':
    save(p, dict(layers=[], tempo=[[0, float(flag('--bpm'))]], clips=[], markers=[]))
elif verb == 'layer add':
    d = load(p)
    if flag('--type') == 'trigger' and '--parent' not in a:
        sys.exit('trigger lanes need --parent')
    d['layers'].append(flag('--name')); save(p, d); print(uuid.uuid4().hex[:8])
elif verb == 'tempo set':
    d = load(p); d['tempo'] = [[q['beat'], q['bpm']] for q in json.load(open(a[3]))]; save(p, d)
elif verb == 'tempo show':
    print(json.dumps(load(p)['tempo']))
elif verb == 'clip add-bulk':
    d = load(p); lay = flag('--layer'); new = json.load(open(flag('--json'))); ids = []
    allowed = {'start_beat', 'duration_beats', 'name', 'path', 'in_point', 'source_duration'}
    for c in new:
        assert set(c) <= allowed, c
        cid = uuid.uuid4().hex[:8]; ids.append(cid)
        d['clips'].append(dict(clip_id=cid, layer=lay, start_beat=c['start_beat'], duration_beats=c['duration_beats']))
    cs = sorted((c for c in d['clips'] if c['layer'] == lay), key=lambda c: c['start_beat'])
    for x, y in zip(cs, cs[1:]):
        if x['start_beat'] + x['duration_beats'] > y['start_beat'] + 1e-9:
            sys.exit('overlap')
    save(p, d); print(json.dumps(ids))
elif verb == 'clip list':
    d = load(p); m = StepMap(d['tempo'])
    print(json.dumps([dict(c, layer_id='x', name=None, start_s=m.seconds(c['start_beat']),
                           end_s=m.seconds(c['start_beat'] + c['duration_beats'])) for c in d['clips']]))
elif verb == 'marker add-bulk':
    d = load(p); d['markers'] += json.load(open(flag('--json'))); save(p, d); print('[]')
elif verb == 'marker list':
    print(json.dumps(load(p)['markers']))
else:
    sys.exit('unknown verb ' + verb)
'''


def song(id_, points, offset_s, dur, **kw):
    return rp.SongPlan(id_, dur, rp.AbletonMap(points), offset_s, 'ableton', **kw)


class TempoMapTests(unittest.TestCase):
    def test_constant_and_step_roundtrip(self):
        m = rp.AbletonMap([(0, 120), (16, 120), (16, 90)])
        self.assertAlmostEqual(m.seconds(16), 8.0)
        self.assertAlmostEqual(m.seconds(25), 8.0 + 9 * 60 / 90)
        for b in (0.0, 3.3, 16.0, 17.25, 40.0):
            self.assertAlmostEqual(m.beat(m.seconds(b)), b, places=9)

    def test_ramp_matches_numeric_integral(self):
        m = rp.AbletonMap([(0, 100), (8, 100), (24, 140)])
        xs = np.linspace(8, 20, 200001)
        bpm = 100 + 40 * (xs - 8) / 16
        want = 4.8 + np.trapezoid(60 / bpm, xs)
        self.assertAlmostEqual(m.seconds(20), want, places=7)
        self.assertAlmostEqual(m.beat(m.seconds(19.7)), 19.7, places=9)

    def test_window_steps_exact_at_every_beat(self):
        m = rp.AbletonMap([(0, 120), (32, 120), (48, 140), (64, 140)])
        steps, worst = rp.window_steps(m, 28, 72)
        pm = rp.StepMap([(b - 28, v) for b, v in steps])
        for b in range(28, 73):
            self.assertAlmostEqual(pm.seconds(b - 28), m.seconds(b) - m.seconds(28), places=9)
        self.assertLess(worst, 1e-4)          # a 120 -> 140 ramp over 16 beats, quarter-beat steps: under 0.1 ms
        self.assertGreater(worst, 0)

    def test_step_change_stays_a_single_step(self):
        m = rp.AbletonMap([(0, 128), (64, 128), (64, 96)])
        steps, worst = rp.window_steps(m, 60, 80)
        self.assertEqual(steps, [(60, 128.0), (64, 96.0)])
        self.assertEqual(worst, 0.0)


class LayoutTests(unittest.TestCase):
    def plans(self):
        a = song('a', [(0, 120), (40, 120), (40, 90)], -30.0, 60.0, locators=[(64, 'drop'), (500, 'far')])
        b = song('b', [(0, 100), (16, 100), (32, 150)], -7.0, 30.0)
        return [a, b]

    def test_windows_on_bars_and_bar_lines_match(self):
        wins, pm = rp.layout(self.plans())
        self.assertEqual(wins[0].start_beat, 0)
        for w in wins:
            self.assertEqual(w.start_beat % 4, 0)
            s = w.song
            self.assertEqual(s.a0_beat % 4, 0)
            self.assertLessEqual(s.a0_beat, s.start_beat)
            self.assertLess(s.start_beat, s.a0_beat + 4)
            for k in range(0, s.mix_bars * 4 + 1):
                b = s.a0_beat + k
                got = pm.seconds(w.start_beat + k) - pm.seconds(w.start_beat)
                self.assertAlmostEqual(got, s.amap.seconds(b) - s.amap.seconds(s.a0_beat), places=9)
        self.assertEqual(wins[1].start_beat, wins[0].end_beat)
        self.assertEqual(wins[0].song.span_beats, 4 * (wins[0].song.mix_bars + rp.GAP_BARS))

    def test_mix_time_maps_through_the_song_map(self):
        wins, pm = rp.layout(self.plans())
        for w in wins:
            s = w.song
            for t in (0.0, 1.234, s.duration_s / 2, s.duration_s):
                b = rp.to_beat(pm, w, t)
                self.assertAlmostEqual(pm.seconds(b), w.mix_proj_s + t, places=9)
                ableton = w.start_beat + s.amap.beat(t - s.offset_s) - s.a0_beat
                self.assertAlmostEqual(b, ableton, delta=1e-3)    # the ramp's quarter-beat steps only

    def test_end_rounds_up_to_a_bar(self):
        s = song('c', [(0, 120)], -1.0, 7.0)   # mix covers arrangement 1..8 s = beats 2..16
        self.assertEqual(s.a0_beat, 0)
        self.assertEqual(s.mix_bars, 4)
        s = song('d', [(0, 120)], -1.0, 7.1)
        self.assertEqual(s.mix_bars, 5)

    def test_lanes_overlap_free_and_markers(self):
        plans = self.plans()
        wins, pm = rp.layout(plans)
        labels = {'kick': {'a': dict(positives=[1.0, 1.001, 1.001, 2.0], dense_pos=[2.01], unscored=[[0.5, 1.5], [1.2, 3.0]],
                                     duration_s=60.0)},
                  'snare': {'b': dict(positives=[5.0], unscored=[[-1.0, 0.5]], duration_s=30.0)}}
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / 'a.json').write_text('\n'.join(json.dumps(dict(family='kick', onset_s=t, emit_s=t + .08))
                                                      for t in (1.0, 1.02, 30.0)))
            lanes, markers, stats = rp.lane_content(wins, pm, labels, d)
        rp.assert_overlap_free(lanes)
        self.assertEqual(stats['a']['kick_labels'], 4)       # one exact duplicate dropped
        self.assertEqual(stats['a']['kick_label_dupes'], 1)
        self.assertEqual(stats['a']['kick_unsure'], 1)       # the two spans merge
        self.assertEqual(stats['a']['kick_fires'], 3)
        self.assertEqual(stats['b']['fires'], 'none')
        fires = sorted(lanes['Kick fires'], key=lambda c: c['start_beat'])
        self.assertAlmostEqual(fires[0]['start_beat'] + fires[0]['duration_beats'], fires[1]['start_beat'])
        names = [m['name'] for m in markers]
        self.assertIn(f'a (bar {int(plans[0].a0_beat) // 4 + 1})', names)
        self.assertIn('drop', names)
        self.assertNotIn('far', names)
        self.assertEqual(lanes['Snare unsure'][0]['meta']['lab_s'], 0.0)


class CheckTests(unittest.TestCase):
    def test_stretches(self):
        fires = set(range(0, 10)) | set(range(20, 30))
        labels = set(range(20, 30)) | set(range(40, 49))
        got = rp.check_stretches(60, fires, labels, set())
        self.assertEqual(got, [(0, 10, 'fires'), (40, 9, 'labels')])

    def test_short_gap_merges_and_unsure_is_ignored(self):
        fires = set(range(0, 5)) | set(range(6, 9))
        self.assertEqual(rp.check_stretches(20, fires, set(), set()), [(0, 9, 'fires')])
        self.assertEqual(rp.check_stretches(20, set(range(10)), set(), set(range(10))), [])
        self.assertEqual(rp.check_stretches(20, set(range(7)), set(), set()), [])


class DiffTests(unittest.TestCase):
    def setUp(self):
        self.side = dict(songs=[dict(id='a', window_start_s=0.0, window_end_s=100.0, window_start_beat=0, window_end_beat=200,
                                     mix_proj_s=10.0),
                                dict(id='b', window_start_s=100.0, window_end_s=200.0, window_start_beat=200,
                                     window_end_beat=400, mix_proj_s=105.0)],
                         clips={'k1': dict(song='a', family='kick', kind='label', lab_s=1.0),
                                'k2': dict(song='a', family='kick', kind='label', lab_s=2.0),
                                'k3': dict(song='a', family='kick', kind='label', lab_s=3.0),
                                'k4': dict(song='a', family='kick', kind='label', lab_s=4.0),
                                's1': dict(song='b', family='snare', kind='label', lab_s=1.0),
                                'u1': dict(song='a', family='kick', kind='unsure', lab_s=20.0, lab_end_s=22.0),
                                'f1': dict(song='a', family='kick', kind='fire', lab_s=5.0)})
        c = lambda cid, lay, beat, s: dict(clip_id=cid, layer=lay, start_beat=beat, start_s=s)  # noqa: E731
        self.orig = [c('k1', 'Kick labels', 22, 11.0), c('k2', 'Kick labels', 24, 12.0), c('k3', 'Kick labels', 26, 13.0),
                     c('k4', 'Kick labels', 28, 14.0), c('s1', 'Snare labels', 212, 106.0), c('f1', 'Kick fires', 30, 15.0),
                     c('u1', 'Kick unsure', 60, 30.0)]
        self.edit = [c('k1', 'Kick labels', 22, 11.0), c('k2', 'Kick labels', 24.1, 12.05),
                     c('k4', 'Snare labels', 28, 14.0), c('s1', 'Snare labels', 212, 106.0),
                     c('u1', 'Kick unsure', 60, 30.0), c('n1', 'Kick labels', 62, 31.0),
                     c('n2', 'Kick fires', 70, 35.0), c('n3', 'Snare labels', 230, 115.0)]

    def test_diff(self):
        out, other, reviewed = rp.diff(self.side, self.orig, self.edit,
                                       [dict(beat=250, name='Reviewed'), dict(beat=10, name='note')])
        k = out['kick']['a']
        self.assertEqual(sorted(k['not_hits']), [3.0, 4.0])          # k3 deleted, k4 moved to the snare lane
        self.assertEqual(len(k['moved']), 1)
        self.assertAlmostEqual(k['moved'][0]['grid_s'], 2.05)
        self.assertEqual(k['moved'][0]['from_s'], 2.0)
        self.assertEqual([h['grid_s'] for h in k['new_hits']], [21.0])
        self.assertEqual(k['unsure_resolved'], [dict(span=[20.0, 22.0], hits=[21.0])])
        self.assertEqual(out['snare']['a']['new_hits'], [dict(grid_s=4.0, from_family='kick')])
        self.assertEqual([h['grid_s'] for h in out['snare']['b']['new_hits']], [10.0])
        self.assertNotIn('a', out['snare'].get('b', {}))
        self.assertEqual(other, {'deleted fire clips': 1, 'new clips on Kick fires': 1})
        self.assertEqual(reviewed, ['b'])


class AttackTests(unittest.TestCase):
    def test_attack_found_near_grid(self):
        sr = 48000
        x = np.random.default_rng(0).standard_normal(sr * 2) * 1e-4
        t0 = 1.0123
        n = np.arange(int(.1 * sr))
        x[int(t0 * sr):int(t0 * sr) + len(n)] += np.sin(2 * np.pi * 200 * n / sr) * np.exp(-n / (.03 * sr))
        got = rp.attack_time(x, sr, 1.0, (100, 400))
        self.assertLess(abs(got - t0), .004)
        self.assertIsNone(rp.attack_time(x, sr, .05, (100, 400)))


class GridFitTests(unittest.TestCase):
    def test_steady_kicks_fit(self):
        k = 0.37 + np.arange(200) * 60 / 126
        bpm, down, note = rp.fit_grid(k)
        self.assertAlmostEqual(bpm, 126, places=1)
        off = (k - down) / (60 / bpm)
        self.assertLess(np.max(np.abs(off - np.round(off))), .02)

    def test_name_bpm_wins_when_it_fits(self):
        k = 0.1 + np.arange(30) * 60 / 128
        self.assertEqual(rp.fit_grid(k, 128.0)[0], 128.0)

    def test_unsteady_kicks_give_no_grid(self):
        k = np.cumsum(np.random.default_rng(1).uniform(.3, .9, 100))
        self.assertIsNone(rp.fit_grid(k)[0])


class EndToEndTests(unittest.TestCase):
    """build -> an edited copy -> read, against the fake project_tool."""

    def test_build_and_read(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            tool = d / 'project_tool'
            tool.write_text(FAKE_TOOL.replace('sys.path.insert(0, ROOT)', f'sys.path.insert(0, {str(rp.ROOT)!r})'))
            tool.chmod(0o755)
            plans = [song('a', [(0, 120), (40, 120), (40, 90)], -30.0, 6.0), song('b', [(0, 100), (16, 100), (32, 150)],
                                                                                  -7.0, 6.0)]
            sr = 48000
            wavs = {}
            for p in plans:
                x = np.random.default_rng(2).standard_normal(int(p.duration_s * sr)) * 1e-4
                wavs[p.id] = d / f'{p.id}.wav'
                import soundfile as sf
                sf.write(str(wavs[p.id]), x.astype(np.float32), sr, subtype='FLOAT')
            labels = {'kick': {'a': dict(positives=[1.0, 2.0], unscored=[[3.0, 4.0]], duration_s=6.0)},
                      'snare': {'b': dict(positives=[1.5], unscored=[], duration_s=6.0)}}
            out = d / 'review.manifold'
            with mock.patch.object(rp, 'plan_songs', lambda only: plans), \
                    mock.patch.object(rp, 'load_labels', lambda: labels), \
                    mock.patch.object(rp, 'write_audio', lambda ps: (wavs, {}, {'a': 'none', 'b': 'none'})), \
                    mock.patch.object(rp, 'online_only_inputs', lambda plans: []), \
                    mock.patch.object(rp, 'drum_stem_status', lambda t: (None, None, 'none')), \
                    mock.patch('builtins.print'):
                rp.build(out, None, None, False, str(tool))
            side = json.loads(rp.sidecar_path(out).read_text())
            self.assertEqual(sorted(m['kind'] for m in side['clips'].values()), ['label', 'label', 'label', 'mix', 'mix', 'unsure'])
            edited = d / 'edited.manifold'
            shutil.copy(out, edited)
            proj = json.loads(edited.read_text())
            kick = [c for c in proj['clips'] if c['layer'] == 'Kick labels']
            proj['clips'].remove(kick[0])
            unsure = next(c for c in proj['clips'] if c['layer'] == 'Kick unsure')
            proj['clips'].append(dict(clip_id='new1', layer='Kick labels', start_beat=unsure['start_beat'] + .5,
                                      duration_beats=.0625))
            proj['markers'].append(dict(beat=side['songs'][0]['window_start_beat'] + 1, name='reviewed'))
            edited.write_text(json.dumps(proj))
            with mock.patch.object(rp, 'family_bands', lambda: {'kick': (100, 400), 'snare': (1000, 8000)}), \
                    mock.patch('builtins.print'):
                rp.read(out, edited, str(tool))
            ov = json.loads((d / 'edited.review' / 'kick_overlay.json').read_text())
            self.assertEqual(ov['provenance'], 'peter')
            self.assertEqual(ov['songs']['a']['not_hits'], [1.0])
            hit = ov['songs']['a']['new_hits'][0]
            beat_s = 60 / 90 * .5                                    # half a beat at the song's 90 BPM
            self.assertAlmostEqual(hit['grid_s'], 3.0 + beat_s, places=6)
            self.assertIn('attack_s', hit)
            self.assertEqual(len(ov['songs']['a']['unsure_resolved']), 1)
            self.assertEqual(ov['reviewed'], ['a'])
            self.assertEqual(ov['unreviewed'], ['b'])
            self.assertEqual(ov['counts']['a']['not_hits'], 1)


def clicks(sr, dur, times, amp=0.8):
    """Silence with a decaying 200 Hz burst at each time (s)."""
    x = np.random.default_rng(3).standard_normal(int(dur * sr)) * 1e-4
    n = np.arange(int(.06 * sr))
    burst = amp * np.sin(2 * np.pi * 200 * n / sr) * np.exp(-n / (.015 * sr))
    for t in times:
        a = int(round(t * sr))
        x[a:a + len(n)] += burst[:len(x) - a]
    return x


MASTER_DB = '''
CREATE TABLE songs(id TEXT PRIMARY KEY, group_id TEXT, group_override TEXT, lab_mix TEXT);
CREATE TABLE assets(path TEXT PRIMARY KEY, name TEXT, role TEXT, variant TEXT, version TEXT, mtime_ns INTEGER,
  local INTEGER, sha256 TEXT, duration_s REAL);
CREATE TABLE comp_assets(comp_id TEXT, path TEXT, via TEXT, flags TEXT, PRIMARY KEY(comp_id, path));
CREATE TABLE variants(song_id TEXT, ref TEXT, path TEXT, lag_s REAL, verdict TEXT, key TEXT);
'''


def master_db(rows):
    """An in-memory catalogue with song 'a' in comp 'c' and the given (path, role, version, day, local, sha) rows."""
    import sqlite3
    con = sqlite3.connect(':memory:')
    con.executescript(MASTER_DB)
    con.execute("INSERT INTO songs VALUES ('a', 'c', NULL, NULL)")
    for path, role, ver, day, local, sha in rows:
        con.execute('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?)', (path, Path(path).name, role, None, ver,
                                                                      day * 86400 * 10**9, local, sha, 200.0))
        con.execute("INSERT INTO comp_assets VALUES ('c', ?, 'release', '[]')", (path,))
    return con


class LatestMasterTests(unittest.TestCase):
    def test_rule(self):
        con = master_db([
            ('/m/x - T1 (mastered).wav', 'master', 'T1 MASTER', 10, 1, 's1'),
            ('/m/x - T2 (mastered).wav', 'master', 'T2 MASTER', 20, 1, 's2'),
            ('/r/x - T2 (mastered).wav', 'master', 'T2 MASTER', 90, 1, 's2'),        # a copy into a release folder
            ('/m/x - T2 (mastered) MP3.mp3', 'master', 'T2 MASTER', 25, 1, 's2mp3'),  # lossy, later
            ('/m/x - PRE MASTER V9.wav', 'master', 'PREMASTER MASTER V9', 30, 1, 'p'),
            ('/m/x STEM MASTER - WIP 6.mp3', 'master', 'WIP 6 MASTER', 30, 0, None),
            ('/m/x - NO MASTER FX.wav', 'master', 'MASTER', 40, 1, 'nofx'),
            ('/m/x - MASTER 4.wav', 'master', 'MASTER', 50, 1, 'm4'),                 # no number: under any T or V
            ('/m/x - FINAL.wav', 'full_mix', 'FINAL', 60, 1, 'f'),
        ])
        m = rp.latest_master(con, 'a')
        self.assertEqual((m['sha256'], m['revision'], m['date']), ('s2', 2, 20 * 86400 * 10**9))
        self.assertEqual(m['local_paths'], ['/m/x - T2 (mastered).wav', '/r/x - T2 (mastered).wav'])

    def test_online_copy_joins_its_local_file_and_a_higher_online_master_still_wins(self):
        con = master_db([('/m/x T2 (mastered).wav', 'master', 'T2 MASTER', 20, 1, 's2'),
                         ('/o/x T2 (mastered).wav', 'master', 'T2 MASTER', 21, 0, None)])
        m = rp.latest_master(con, 'a')
        self.assertTrue(m['local'])
        self.assertEqual(len(m['paths']), 2)
        con = master_db([('/m/x T2 (mastered).wav', 'master', 'T2 MASTER', 20, 1, 's2'),
                         ('/o/x T3 (mastered).wav', 'master', 'T3 MASTER', 21, 0, None)])
        self.assertFalse(rp.latest_master(con, 'a')['local'])

    def test_remaster_counts_and_no_master_is_none(self):
        con = master_db([('/m/x MASTER 4.wav', 'master', 'MASTER', 20, 1, 'm4'),
                         ('/m/x [REMASTER FINAL].wav', 'full_mix', 'FINAL', 21, 1, 'rf'),
                         ('/m/x Remaster WIP 8.mp3', 'full_mix', 'WIP 8', 30, 1, 'w8')])
        self.assertEqual(rp.latest_master(con, 'a')['sha256'], 'rf')
        self.assertIsNone(rp.latest_master(master_db([('/m/x WIP 3.mp3', 'full_mix', 'WIP 3', 1, 1, 'w')]), 'a'))


class AudioPlanTests(unittest.TestCase):
    """Which audio a song plays, from the latest master and the proofs."""

    def setUp(self):
        from tools.audio_analysis.lab import registry
        self.d = tempfile.TemporaryDirectory()
        d = Path(self.d.name)
        self.t2, self.t2b = str(d / 'x T2 (mastered).wav'), str(d / 'x T2 copy (mastered).wav')
        for p in (self.t2, self.t2b):
            Path(p).write_bytes(b'x')
        self.con = master_db([(self.t2, 'master', 'T2 MASTER', 20, 1, 's2')])
        self.song = registry.Song('a', 'project', dict(how='goal', paths=['/lab/a.npy']), ('labsha',), 48000, -30.0,
                                  'lab', ('kick',))

    def tearDown(self):
        self.d.cleanup()

    def plan(self, rows):
        with mock.patch.object(rp, 'review_proof', lambda s, m, con: dict(status='not proven [review proof]')):
            return rp.audio_plan(self.song, self.con, {'a': rows})

    def row(self, vid, sha, status, lag=None, path=None):
        r = dict(id=vid, parent='a', sha256=sha, status=status, lag_s=lag, path=path or self.t2)
        if status.startswith('proven'):
            r['offset_s'] = self.song.offset_s - lag
        return r

    def test_proven_master_moves_by_minus_the_lag(self):
        a = self.plan([self.row('a@mix-s2', 's2', 'proven (kick and snare agree)', lag=-0.0125)])
        self.assertEqual((a.source, a.path, a.sha256), ('master', self.t2, 's2'))
        self.assertAlmostEqual(a.shift_s, 0.0125, places=12)
        self.assertEqual(a.label, 'master T2 MASTER')

    def test_same_audio_follows_to_the_proven_file(self):
        a = self.plan([self.row('a@mix-s2', 's2', 'excluded (same audio as a@mix-s9)'),
                       self.row('a@mix-s9', 's9', 'proven (relative: x)', lag=0.002, path=self.t2b)])
        self.assertEqual(a.source, 'master')
        self.assertAlmostEqual(a.shift_s, -0.002, places=12)
        Path(self.t2).unlink()           # the chosen copy evicted: the proven file with the same audio is read instead
        a = self.plan([self.row('a@mix-s2', 's2', 'excluded (same audio as a@mix-s9)'),
                       self.row('a@mix-s9', 's9', 'proven (relative: x)', lag=0.002, path=self.t2b)])
        self.assertEqual((a.path, a.sha256), (self.t2b, 's9'))

    def test_kept_with_a_reason(self):
        a = self.plan([self.row('a@mix-s2', 's2', 'not proven (fit -29.07 s, labels use -29.22 s)')])
        self.assertEqual((a.source, a.shift_s), ('labelled', 0.0))
        self.assertEqual(a.label, 'lab mix: master not proven')
        a = self.plan([])
        self.assertIn('review proof', a.why)
        a = self.plan([self.row('a@mix-s2', 's2', 'excluded (identical audio to the lab mix)')])
        self.assertEqual((a.source, a.label), ('labelled', 'lab mix: the labelled file is the latest master'))
        self.assertEqual(rp.audio_plan(self.song, master_db([]), {}).label, 'lab mix: no master in the catalogue')
        on = master_db([('/o/x T3 (mastered).wav', 'master', 'T3 MASTER', 21, 0, None)])
        self.assertEqual(rp.audio_plan(self.song, on, {}).label, 'lab mix: master online-only')


class ShiftSignTests(unittest.TestCase):
    """The catalogue's lag, the proof's offset and the review's shift agree in sign: a master that starts D seconds
    later than the labelled file plays a label at t at t + D."""

    def test_sign_end_to_end(self):
        from tools.audio_analysis.catalog import variants
        from tools.audio_analysis.lab import registry
        sr, d = 32000, 0.4375
        times = list(np.cumsum(np.random.default_rng(5).uniform(.2, .6, 140)) + 1.0)
        lab = clicks(sr, 70.0, times)
        master = np.concatenate([np.zeros(int(round(d * sr))), lab])
        lag = variants.compare(lab, master, sr)['lag_s']
        self.assertAlmostEqual(lag, -d, places=6)                    # catalogue: master time = lab time - lag_s
        song = registry.Song('a', 'project', dict(how='goal', paths=['x']), ('s',), sr, -12.0, 'lab', ('kick',))
        rec = dict(path='m.wav', sha256='m' * 64, offset_s=song.offset_s - lag, status='proven')   # offsets.prove_variant
        shift = registry.variant_shift(registry.variant_copy(song, rec), song)
        self.assertAlmostEqual(shift, d, places=6)
        for t in times[10:15]:
            self.assertLess(abs(rp.attack_time(master, sr, t + shift, (100, 400)) - (t + d)), .004)


class MovedGridTests(unittest.TestCase):
    def test_bar_lines_follow_the_labels_onto_the_master(self):
        sh = 0.3125
        plan = rp.on_audio(song('a', [(0, 120), (40, 120), (40, 90)], -30.0, 60.0),
                           rp.AudioPlan('master', 'm.wav', 'm', sh, master=dict(version='T2'), duration_s=60.3125))
        self.assertEqual((plan.offset_s, plan.duration_s), (-30.0 + sh, 60.3125))
        ref = song('a', [(0, 120), (40, 120), (40, 90)], -30.0, 60.0)
        wins, pm = rp.layout([plan])
        w = wins[0]
        for t in (0.5, 7.25, 31.0):                       # a label at labelled second t, on the master at t + sh
            b = rp.to_beat(pm, w, t + sh)
            self.assertAlmostEqual(b - w.start_beat, ref.amap.beat(t - ref.offset_s) - plan.a0_beat, places=9)
        labels = {'kick': {'a': dict(positives=[-0.5, 1.0, 59.9], unscored=[], duration_s=60.0)}, 'snare': {}}
        lanes, markers, stats = rp.lane_content(wins, pm, labels, None)
        self.assertEqual([c['meta']['lab_s'] for c in lanes['Kick labels']], [1.0, 59.9])
        self.assertEqual(stats['a']['kick_outside'], 1)               # -0.5 + 0.3125 is before the master starts
        self.assertTrue(markers[0]['name'].endswith('[master T2]'))


class StereoTests(unittest.TestCase):
    def test_read_stereo_folds_to_the_lab_reader(self):
        import soundfile as sf
        from scipy.io import wavfile

        from tools.audio_analysis.lab import remix
        rng = np.random.default_rng(7)
        x = (rng.standard_normal((44100, 2)) * 3000).astype(np.int16)
        with tempfile.TemporaryDirectory() as d:
            w, f = Path(d) / 's.wav', Path(d) / 's.flac'
            wavfile.write(w, 44100, x)
            sf.write(str(f), x.astype(np.float64) / 32768, 44100, subtype='PCM_16')
            for p in (w, f):
                st = rp.read_stereo(p, 48000)
                self.assertEqual(st.shape[1], 2)
                np.testing.assert_allclose(st.mean(axis=1), remix.read_mono(p, 48000), atol=1e-12)

    def test_labelled_stereo_keeps_only_the_same_audio(self):
        rng = np.random.default_rng(8)
        st = rng.standard_normal((48000, 2)) * .1
        ref = st.mean(axis=1)
        s = SimpleSong()
        with mock.patch.object(rp, 'stereo_sources', lambda song, con: (['a', 'b'], None)), \
                mock.patch.object(rp, 'read_stereo', lambda p, sr: st / 2 if p == 'a' else st / 2):
            got, why = rp.labelled_stereo(s, ref, None)
            self.assertIsNone(why)
            np.testing.assert_allclose(got, st)
        with mock.patch.object(rp, 'stereo_sources', lambda song, con: (['a'], None)), \
                mock.patch.object(rp, 'read_stereo', lambda p, sr: np.roll(st, 3, axis=0)):
            got, why = rp.labelled_stereo(s, ref, None)
            self.assertIsNone(got)
            self.assertIn('does not fold back', why)

    def test_mono_part_goes_to_every_channel(self):
        out = rp.add_channels(np.ones((4, 2)), np.full((6, 1), 2.0))
        self.assertEqual(out.shape, (6, 2))
        np.testing.assert_allclose(out.mean(axis=1), [3, 3, 3, 3, 2, 2])
        np.testing.assert_allclose(rp.shift_channels(np.arange(8.).reshape(4, 2), 1, 4)[:, 1], [0, 1, 3, 5])


class SimpleSong:
    sample_rate = 48000


class MasterRoundTripTests(unittest.TestCase):
    """build on a master timeline -> edits -> read: every time comes back in the labelled file's seconds."""

    def test_round_trip(self):
        sh = 0.75
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            tool = d / 'project_tool'
            tool.write_text(FAKE_TOOL.replace('sys.path.insert(0, ROOT)', f'sys.path.insert(0, {str(rp.ROOT)!r})'))
            tool.chmod(0o755)
            plan = rp.on_audio(song('a', [(0, 120)], -10.0, 8.0),
                               rp.AudioPlan('master', 'm.wav', 'm', sh, master=dict(version='T2'), duration_s=8.75))
            sr = 48000
            kicks = [1.0, 2.0, 3.5]
            x = clicks(sr, 8.75, [t + sh for t in kicks])            # the master: the labelled hits, 0.75 s later
            wav = d / 'a.wav'
            import soundfile as sf
            sf.write(str(wav), np.stack([x, x], axis=1).astype(np.float32), sr, subtype='FLOAT')
            labels = {'kick': {'a': dict(positives=kicks, unscored=[[5.0, 6.0]], duration_s=8.0)}, 'snare': {}}
            out = d / 'review.manifold'
            with mock.patch.object(rp, 'plan_songs', lambda only: [plan]), \
                    mock.patch.object(rp, 'load_labels', lambda: labels), \
                    mock.patch.object(rp, 'online_only_inputs', lambda plans: []), \
                    mock.patch.object(rp, 'write_audio', lambda ps: ({'a': wav}, {}, {'a': 'none'})), \
                    mock.patch.object(rp, 'drum_stem_status', lambda t: (None, None, 'none')), \
                    mock.patch('builtins.print'):
                rp.build(out, None, None, False, str(tool))
            side = json.loads(rp.sidecar_path(out).read_text())
            self.assertEqual((side['version'], side['songs'][0]['shift_s']), (2, sh))
            self.assertEqual(side['songs'][0]['audio']['source'], 'master')
            self.assertEqual(sorted(m['lab_s'] for m in side['clips'].values() if m['kind'] == 'label'), kicks)
            proj = json.loads(out.read_text())
            pm = rp.StepMap(proj['tempo'])
            kick = {round(pm.seconds(c['start_beat']) - side['songs'][0]['mix_proj_s'], 6): c for c in proj['clips']
                    if c['layer'] == 'Kick labels'}
            self.assertEqual(sorted(kick), [t + sh for t in kicks])  # on the master, each label sits on its hit
            edited = d / 'edited.manifold'
            kick[1.0 + sh]['start_beat'] += 0.02                      # nudged 10 ms (120 BPM)
            proj['clips'].remove(kick[2.0 + sh])                      # not a hit
            proj['clips'].append(dict(clip_id='new1', layer='Kick labels', start_beat=kick[3.5 + sh]['start_beat'],
                                      duration_beats=.0625))          # a new hit exactly on an existing label
            unsure = next(c for c in proj['clips'] if c['layer'] == 'Kick unsure')
            proj['clips'].append(dict(clip_id='new2', layer='Kick labels', start_beat=unsure['start_beat'] + 1.0,
                                      duration_beats=.0625))          # a hit inside the unsure span
            edited.write_text(json.dumps(proj))
            with mock.patch.object(rp, 'family_bands', lambda: {'kick': (100, 400), 'snare': (1000, 8000)}), \
                    mock.patch('builtins.print'):
                rp.read(out, edited, str(tool))
            ov = json.loads((d / 'edited.review' / 'kick_overlay.json').read_text())['songs']['a']
            self.assertEqual(ov['not_hits'], [2.0])
            self.assertAlmostEqual(ov['moved'][0]['grid_s'], 1.01, places=6)
            self.assertEqual(ov['moved'][0]['from_s'], 1.0)
            self.assertLess(abs(ov['moved'][0]['attack_s'] - 1.0), .004)   # the attack is found back on the hit
            news = sorted(h['grid_s'] for h in ov['new_hits'])
            self.assertAlmostEqual(news[0], 3.5, places=6)
            self.assertAlmostEqual(news[1], 5.5, places=6)                # unsure span 5..6 at 120 BPM, +1 beat
            self.assertEqual(ov['unsure_resolved'][0]['span'], [5.0, 6.0])


if __name__ == '__main__':
    unittest.main()
