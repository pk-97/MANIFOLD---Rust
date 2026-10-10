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

from tools.audio_analysis.eval.als_extract import beat_seconds, extract, sha256, unroll, warp_seconds  # noqa: E402

MIDI_TRACK = """
  <MidiTrack Id="3"><Name><EffectiveName Value="KICK"/></Name><TrackGroupId Value="1"/><TrackDelay><Value Value="0"/></TrackDelay>
   <Freeze Value="false"/>
   <DeviceChain><Mixer><Speaker><Manual Value="true"/></Speaker><SoloSink Value="false"/></Mixer>
    <DeviceChain><Devices>
     <MidiArpeggiator><UserName Value=""/><On><Manual Value="false"/></On></MidiArpeggiator>
     <DrumGroupDevice><UserName Value="Kit"/><On><Manual Value="true"/></On><Branches>
      <DrumBranch><Name><EffectiveName Value="Kick 909"/></Name><BranchInfo><ReceivingNote Value="92"/></BranchInfo>
       <DeviceChain><MidiToAudioDeviceChain><Devices><OriginalSimpler><UserName Value=""/>
        <Player><MultiSampleMap><SampleParts><MultiSamplePart><SampleRef><FileRef><Path Value="/s/kick_909.wav"/></FileRef></SampleRef>
        </MultiSamplePart></SampleParts></MultiSampleMap></Player></OriginalSimpler></Devices></MidiToAudioDeviceChain></DeviceChain>
      </DrumBranch>
     </Branches></DrumGroupDevice>
    </Devices></DeviceChain>
    <MainSequencer>
     <ClipTimeable><ArrangerAutomation><Events>
      <MidiClip Time="0"><CurrentStart Value="0"/><CurrentEnd Value="10"/><Disabled Value="false"/><Name Value="loop"/>
       <Loop><LoopStart Value="0"/><LoopEnd Value="4"/><StartRelative Value="2"/><LoopOn Value="true"/></Loop>
       <Notes><KeyTracks><KeyTrack><MidiKey Value="36"/><Notes>
        <MidiNoteEvent Time="0" Duration="0.5" Velocity="100"/><MidiNoteEvent Time="3" Duration="0.5" Velocity="90"/>
        <MidiNoteEvent Time="1" Duration="0.5" Velocity="90" IsEnabled="false"/>
       </Notes></KeyTrack></KeyTracks></Notes>
      </MidiClip>
      <MidiClip Time="12"><CurrentStart Value="12"/><CurrentEnd Value="16"/><Disabled Value="true"/><Name Value="off"/>
       <Loop><LoopStart Value="0"/><LoopEnd Value="4"/><StartRelative Value="0"/><LoopOn Value="false"/></Loop>
       <Notes><KeyTracks><KeyTrack><MidiKey Value="36"/><Notes><MidiNoteEvent Time="0" Duration="1" Velocity="100"/></Notes></KeyTrack></KeyTracks></Notes>
      </MidiClip>
     </Events></ArrangerAutomation></ClipTimeable>
     <ClipSlotList><ClipSlot><ClipSlot><Value/></ClipSlot></ClipSlot><ClipSlot><ClipSlot><Value>
      <MidiClip Time="0"><Disabled Value="false"/><Name Value="fill"/>
       <Loop><LoopStart Value="0"/><LoopEnd Value="1"/><StartRelative Value="0"/><LoopOn Value="true"/></Loop>
       <Notes><KeyTracks><KeyTrack><MidiKey Value="38"/><Notes><MidiNoteEvent Time="0.5" Duration="0.25" Velocity="70" Probability="0.5"/></Notes></KeyTrack></KeyTracks></Notes>
      </MidiClip></Value></ClipSlot></ClipSlot></ClipSlotList>
    </MainSequencer></DeviceChain>
  </MidiTrack>"""

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
  </AudioTrack>{midi}
 </Tracks>
 <Scenes><Scene><Name Value="Intro"/></Scene><Scene><Name Value="Drop"/></Scene></Scenes>
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
        f.write(SET.format(main=main, midi=MIDI_TRACK).encode())
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

    def test_unroll_unlooped_clip_starts_at_marker(self):
        clip = dict(clip_start_beat=1.0, looped=False, loop_start=0.0, loop_end=4.0)
        notes = [dict(time=t) for t in (0.0, 1.0, 2.5, 3.9)]
        self.assertEqual([b for b, _ in unroll(clip, notes, 10.0, 12.0)], [10.0, 11.5])

    def test_midi_devices_and_session(self):
        with tempfile.TemporaryDirectory() as d:
            res = extract(write_set(d, 'MainTrack'))
        kick = next(t for t in res['tracks'] if t['name'] == 'KICK')
        self.assertEqual(kick['groups'], ['2 DRUMS'])
        loop, off = kick['midi_clips']
        # loop 0-4 entered at clip beat 2: the note at 3, then whole passes from beat 2
        self.assertEqual([n['beat'] for n in loop['notes']], [1.0, 2.0, 5.0, 6.0, 9.0])
        self.assertEqual([n['s'] for n in loop['notes']], [.5, 1.0, 2.5, 3.0, 4.5])
        self.assertEqual(loop['notes_disabled'], 1)
        self.assertEqual(off['notes'], [])
        self.assertEqual(kick['midi_effects'], ['MidiArpeggiator'])
        arp, kit = kick['devices']
        self.assertFalse(arp['on'])
        pad, = kit['branches']
        self.assertEqual((pad['name'], pad['key']), ('Kick 909', 36))
        self.assertEqual(pad['devices'][0]['samples'], ['kick_909.wav'])
        fill, = kick['session_clips']
        self.assertEqual((fill['scene'], fill['scene_name'], fill['notes'][0]['key'], fill['notes'][0]['probability']), (1, 'Drop', 38, .5))


if __name__ == '__main__':
    unittest.main()
