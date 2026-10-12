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


if __name__ == '__main__':
    unittest.main()
