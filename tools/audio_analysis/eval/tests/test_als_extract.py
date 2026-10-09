#!/usr/bin/env python3
"""als_extract on a synthetic Live set: tempo-map seconds, warp mapping, clip offsets, read-only."""
from __future__ import annotations

import gzip
import math
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4]))

from tools.audio_analysis.eval.als_extract import beat_seconds, extract, sha256, warp_seconds  # noqa: E402

SET = """<?xml version="1.0" encoding="UTF-8"?>
<Ableton><LiveSet>
 <Tracks>
  <GroupTrack Id="1"><Name><EffectiveName Value="2 DRUMS"/></Name><TrackGroupId Value="-1"/></GroupTrack>
  <AudioTrack Id="2"><Name><EffectiveName Value="MASTER A"/></Name><TrackGroupId Value="1"/>
   <DeviceChain><MainSequencer><Sample><ArrangerAutomation><Events>
    <AudioClip Time="8">
     <CurrentStart Value="8"/><CurrentEnd Value="16"/><Disabled Value="false"/><IsWarped Value="true"/>
     <Loop><LoopStart Value="4"/><LoopEnd Value="100"/><StartRelative Value="0"/><LoopOn Value="false"/></Loop>
     <SampleRef><FileRef><Path Value="/x/song.wav"/></FileRef></SampleRef>
     <WarpMarkers><WarpMarker SecTime="0.25" BeatTime="0"/><WarpMarker SecTime="0.2642045454545" BeatTime="0.03125"/></WarpMarkers>
    </AudioClip>
    <AudioClip Time="20">
     <CurrentStart Value="20"/><CurrentEnd Value="24"/><Disabled Value="true"/><IsWarped Value="true"/>
     <Loop><LoopStart Value="0"/><LoopEnd Value="4"/><StartRelative Value="0"/><LoopOn Value="false"/></Loop>
     <SampleRef><FileRef><Path Value="/x/rec.wav"/></FileRef></SampleRef>
     <WarpMarkers><WarpMarker SecTime="0" BeatTime="0"/><WarpMarker SecTime="1" BeatTime="2"/><WarpMarker SecTime="3" BeatTime="4"/></WarpMarkers>
    </AudioClip>
   </Events></ArrangerAutomation></Sample></MainSequencer></DeviceChain>
  </AudioTrack>
 </Tracks>
 <{main}><DeviceChain><Mixer><Tempo><Manual Value="120"/><AutomationTarget Id="77"/></Tempo></Mixer>
  <AutomationEnvelopes><Envelopes><AutomationEnvelope><EnvelopeTarget><PointeeId Value="77"/></EnvelopeTarget>
   <Automation><Events>
    <FloatEvent Time="-63072000" Value="120"/><FloatEvent Time="0" Value="120"/>
    <FloatEvent Time="16" Value="120"/><FloatEvent Time="16" Value="60"/><FloatEvent Time="24" Value="120"/>
   </Events></Automation></AutomationEnvelope></Envelopes></AutomationEnvelopes></DeviceChain></{main}>
</LiveSet></Ableton>"""


def write_set(folder, main):
    path = Path(folder) / 'show.als'
    with gzip.open(path, 'wb') as f:
        f.write(SET.format(main=main).encode())
    return path


class AlsExtractTest(unittest.TestCase):
    def test_tempo_map_seconds_with_step_and_ramp(self):
        pts = [(0.0, 120.0), (16.0, 120.0), (16.0, 60.0), (24.0, 120.0)]
        self.assertAlmostEqual(beat_seconds(pts, 16), 8.0)
        # 60 -> 120 BPM linear over 8 beats: 60 * 8 / 60 * ln 2
        self.assertAlmostEqual(beat_seconds(pts, 24), 8.0 + 8 * math.log(2))
        self.assertAlmostEqual(beat_seconds(pts, 26), 8.0 + 8 * math.log(2) + 1.0)

    def test_warp_two_markers_extrapolate_constant_tempo(self):
        m = [(0.25, 0.0), (0.2642045454545, 0.03125)]
        self.assertAlmostEqual(warp_seconds(m, 4), 0.25 + 4 * 60 / 132, places=6)

    def test_warp_many_markers_piecewise(self):
        m = [(0.0, 0.0), (1.0, 2.0), (3.0, 4.0)]
        self.assertAlmostEqual(warp_seconds(m, 1), .5)
        self.assertAlmostEqual(warp_seconds(m, 3), 2.0)
        self.assertAlmostEqual(warp_seconds(m, 5), 4.0)

    def test_extract_clips_grids_and_read_only(self):
        for main in ('MainTrack', 'MasterTrack'):
            with tempfile.TemporaryDirectory() as d:
                path = write_set(d, main)
                before = sha256(path)
                res = extract(path)
                self.assertEqual(sha256(path), before)
                song, rec = res['audio_clips']
                self.assertEqual(song['groups'], ['2 DRUMS'])
                self.assertAlmostEqual(song['start_s'], 4.0)
                self.assertAlmostEqual(song['file_start_s'], 0.25 + 4 * 60 / 132, places=6)
                self.assertTrue(rec['disabled'])
                self.assertAlmostEqual(rec['start_s'], 8.0 + 4 * math.log(1.5) * 2, places=6)
                self.assertEqual(list(res['grids']), ['/x/song.wav'])
                self.assertAlmostEqual(res['grids']['/x/song.wav']['bpm'], 132.0, places=3)
                self.assertEqual([p['beat'] for p in res['tempo_points']], [0.0, 16.0, 16.0, 24.0])


if __name__ == '__main__':
    unittest.main()
