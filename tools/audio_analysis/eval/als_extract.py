#!/usr/bin/env python3
"""Read-only timing extractor for Ableton Live sets (.als).

Usage: als_extract.py PROJECT.als [OUT.json]
       (default OUT: ~/.cache/manifold/ableton/<project stem>.json)

The .als is gzip XML; it is only ever opened for reading, and its sha256 is
taken before and after the read (a mismatch aborts without writing).
Emits:
- tempo_points: the arrangement tempo map in the TempoPoint shape that
  eval/beat_scoring.load_tempo_points reads (beat, bpm, recorded_at_seconds,
  source = SOURCE_ALS). Tempo automation is linear in BPM between breakpoints;
  seconds are integrated from beat 0.
- audio_clips: every arrangement audio clip with its track, group chain,
  arrangement span (beats and seconds), source file, the file second playing
  at the clip start, and its warp markers (file seconds <-> clip beats).
- grids: per source file warped with exactly two markers (a constant-tempo
  warp), the file's BPM and the file second of clip beat 0. Warp markers sit
  a few ms off the true attack (Pattern -8 ms, Midnight Patience ~-20 ms
  against their kick stems); measure a per-song nudge from audio before use.
- tracks: every track with its mute (speaker), solo, track delay, freeze,
  device tree (plugins, samples, rack and drum-rack branches with their MIDI
  keys), MIDI effects, arrangement MIDI clips with every played note unrolled
  to beats and seconds (loops, switched-off clips and disabled notes applied),
  and session clips by scene.
Written notes are not what sounded when a MIDI effect (arpeggiator, chord,
random) sits on the track, a note has probability < 1, or groove applies;
those are reported, not resolved. Automation other than tempo is not read.
"""
from __future__ import annotations

import gzip
import hashlib
import json
import math
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

SOURCE_ALS = 10
SENTINEL_BEAT = -1e6  # Live stores the pre-arrangement tempo at beat -63072000


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def val(node, path, cast=str, default=None):
    e = node.find(path)
    return cast(e.get('Value')) if e is not None and e.get('Value') is not None else default


def tempo_points(live_set):
    """[(beat, bpm)] from the main track's tempo and its arrangement automation."""
    main = live_set.find('MainTrack')
    if main is None:
        main = live_set.find('MasterTrack')
    tempo = main.find('.//Tempo')
    manual = val(tempo, 'Manual', float)
    target = tempo.find('AutomationTarget').get('Id')
    pts = []
    for env in main.findall('.//AutomationEnvelopes/Envelopes/AutomationEnvelope'):
        if val(env, 'EnvelopeTarget/PointeeId') == target:
            pts = [(float(e.get('Time')), float(e.get('Value'))) for e in env.findall('Automation/Events/FloatEvent')]
    if not pts:
        return [(0.0, manual)]
    pts.sort(key=lambda p: p[0])
    head = [p for p in pts if p[0] <= SENTINEL_BEAT]
    body = [p for p in pts if p[0] > SENTINEL_BEAT]
    if not body or body[0][0] > 0:
        body.insert(0, (0.0, head[-1][1] if head else manual))
    return body


def beat_seconds(points, beat):
    """Seconds at a beat (>= 0) on a piecewise-linear-BPM tempo map."""
    t = 0.0
    for i, (b0, v0) in enumerate(points):
        if beat <= b0:
            break
        b1, v1 = points[i + 1] if i + 1 < len(points) else (math.inf, v0)
        end = min(beat, b1)
        v_end = v0 + (v1 - v0) * (end - b0) / (b1 - b0) if b0 < b1 < math.inf else v0
        span = end - b0
        t += 60 * span / v0 if abs(v_end - v0) < 1e-9 else 60 * span / (v_end - v0) * math.log(v_end / v0)
    return t


def warp_seconds(markers, clip_beat):
    """File seconds at a clip beat; linear between markers, extrapolated past the ends."""
    if len(markers) == 1:
        return markers[0][0]
    for (s0, b0), (s1, b1) in zip(markers, markers[1:]):
        if clip_beat <= b1:
            break
    return s0 + (clip_beat - b0) * (s1 - s0) / (b1 - b0)


def warp_beats(markers, sec):
    """Clip beat at a file second; the inverse of warp_seconds."""
    if len(markers) == 1:
        return markers[0][1]
    for (s0, b0), (s1, b1) in zip(markers, markers[1:]):
        if sec <= s1:
            break
    return b0 + (sec - s0) * (b1 - b0) / (s1 - s0)


def clip_seconds(clip, arr_beat):
    """File second a clip plays at an arrangement beat (unlooped clips)."""
    if not clip['warped']:
        raise ValueError('unwarped clip: needs the tempo map, not markers')
    return warp_seconds(clip['markers'], clip['clip_start_beat'] + arr_beat - clip['start_beat'])


def clip_arr_beat(clip, sec):
    """Arrangement beat at which a clip plays a file second (unlooped clips)."""
    return clip['start_beat'] + warp_beats(clip['markers'], sec) - clip['clip_start_beat']


def group_chain(tracks_by_id, group_id):
    names = []
    while group_id not in (None, -1) and group_id in tracks_by_id:
        tr = tracks_by_id[group_id]
        names.append(val(tr, 'Name/EffectiveName'))
        group_id = val(tr, 'TrackGroupId', int)
    return names


def file_path(ref, als_dir):
    if ref is None:
        return None
    path = val(ref, 'Path')
    if not path:
        rel = val(ref, 'RelativePath')
        path = str((als_dir / rel).resolve()) if rel else None
    return path


def clip_common(c):
    """Fields every clip shares. Clip time is in beats. Unlooped clips play from the
    start marker (LoopStart + StartRelative); looped ones wrap LoopEnd -> LoopStart."""
    loop_start = val(c, 'Loop/LoopStart', float)
    return dict(name=val(c, 'Name'), disabled=val(c, 'Disabled') == 'true', looped=val(c, 'Loop/LoopOn') == 'true',
                loop_start=loop_start, loop_end=val(c, 'Loop/LoopEnd', float),
                clip_start_beat=loop_start + val(c, 'Loop/StartRelative', float, 0.0))


def audio_clip(c, als_dir):
    path = file_path(c.find('SampleRef/FileRef'), als_dir)
    markers = sorted(((float(w.get('SecTime')), float(w.get('BeatTime'))) for w in c.findall('WarpMarkers/WarpMarker')),
                     key=lambda m: m[1])
    row = dict(clip_common(c), file=Path(path).name if path else None, file_path=path,
               warped=val(c, 'IsWarped') == 'true', markers=markers)
    row['file_start_s'] = warp_seconds(markers, row['clip_start_beat']) if row['warped'] and markers else row['clip_start_beat']
    return row


def midi_notes(c):
    """Enabled notes in clip time. Probability below 1 makes a render non-deterministic; it is kept per note."""
    out, off = [], 0
    for kt in c.findall('Notes/KeyTracks/KeyTrack'):
        key = val(kt, 'MidiKey', int)
        for n in kt.findall('Notes/MidiNoteEvent'):
            if n.get('IsEnabled') == 'false':
                off += 1
                continue
            out.append(dict(key=key, time=float(n.get('Time')), duration=float(n.get('Duration')),
                            velocity=float(n.get('Velocity')), probability=float(n.get('Probability', 1))))
    out.sort(key=lambda n: (n['time'], n['key']))
    return out, off


def unroll(clip, notes, start, end):
    """Arrangement beat of every note the clip plays between its start and end beats."""
    pos, out = clip['clip_start_beat'], []
    if not clip['looped']:
        return [(start + n['time'] - pos, n) for n in notes if pos <= n['time'] < clip['loop_end'] and start + n['time'] - pos < end]
    length = clip['loop_end'] - clip['loop_start']
    if length <= 0:
        return out
    first = [(start + n['time'] - pos, n) for n in notes if pos <= n['time'] < clip['loop_end']]
    out += [x for x in first if x[0] < end]
    t0 = start + clip['loop_end'] - pos
    body = [n for n in notes if clip['loop_start'] <= n['time'] < clip['loop_end']]
    while t0 < end and body:
        out += [(t0 + n['time'] - clip['loop_start'], n) for n in body if t0 + n['time'] - clip['loop_start'] < end]
        t0 += length
    return out


def devices(container, als_dir, depth=0):
    """Device chain as a tree: kind (XML tag), name, on, plugin, samples, rack branches.
    Drum-rack branches carry the MIDI key they answer to (Live stores 128 - key)."""
    out = []
    for d in list(container) if container is not None else []:
        plugin = next((val(d, p) for p in ('PluginDesc/Vst3PluginInfo/Name', 'PluginDesc/VstPluginInfo/PlugName',
                                            'PluginDesc/AuPluginInfo/Name') if val(d, p)), None)
        row = dict(kind=d.tag, name=val(d, 'UserName') or plugin or d.tag, on=val(d, 'On/Manual') != 'false', plugin=plugin)
        if d.tag in ('OriginalSimpler', 'MultiSampler'):
            row['samples'] = sorted({Path(p).name for p in (file_path(r, als_dir) for r in d.iter('FileRef')) if p})
        branches = d.find('Branches')
        if branches is not None and depth < 6:
            row['branches'] = []
            for b in branches:
                note = val(b, 'BranchInfo/ReceivingNote', int)
                chain = next((x for x in b.findall('DeviceChain/*/Devices')), None)
                row['branches'].append(dict(name=val(b, 'Name/EffectiveName') or val(b, 'Name'), key=128 - note if note is not None else None,
                                            on=val(b, 'MixerDevice/Speaker/Manual') != 'false', devices=devices(chain, als_dir, depth + 1)))
        out.append(row)
    return out


def tracks(live_set, als_dir, points):
    """Every track: flags, device tree, arrangement MIDI clips with unrolled notes, session clips by scene."""
    all_tracks = list(live_set.find('Tracks'))
    by_id = {int(t.get('Id')): t for t in all_tracks}
    scenes = [val(s, 'Name') for s in live_set.findall('Scenes/Scene')]
    out = []
    for tr in all_tracks:
        dev = devices(tr.find('DeviceChain/DeviceChain/Devices'), als_dir)
        row = dict(id=int(tr.get('Id')), kind=tr.tag, name=val(tr, 'Name/EffectiveName'),
                   groups=group_chain(by_id, val(tr, 'TrackGroupId', int)),
                   speaker_on=val(tr, 'DeviceChain/Mixer/Speaker/Manual') != 'false', solo=val(tr, 'DeviceChain/Mixer/SoloSink') == 'true',
                   track_delay=val(tr, 'TrackDelay/Value', float, 0.0), frozen=val(tr, 'Freeze') == 'true', devices=dev,
                   midi_effects=[d['name'] for d in dev if d['kind'].startswith('Midi')], midi_clips=[], session_clips=[])
        for c in tr.findall('DeviceChain/MainSequencer/ClipTimeable/ArrangerAutomation/Events/MidiClip'):
            clip = clip_common(c)
            start, end = val(c, 'CurrentStart', float), val(c, 'CurrentEnd', float)
            notes, off = midi_notes(c)
            played = [] if clip['disabled'] else unroll(clip, notes, start, end)
            row['midi_clips'].append(dict(clip, start_beat=start, end_beat=end, start_s=beat_seconds(points, start),
                                          end_s=beat_seconds(points, end), notes_in_clip=len(notes), notes_disabled=off,
                                          notes=[dict(beat=b, s=beat_seconds(points, b), key=n['key'], velocity=n['velocity'],
                                                      duration=n['duration'], probability=n['probability']) for b, n in played]))
        for scene, slot in enumerate(tr.findall('DeviceChain/MainSequencer/ClipSlotList/ClipSlot')):
            for c in slot.findall('ClipSlot/Value/*'):
                base = dict(scene=scene, scene_name=scenes[scene] if scene < len(scenes) else None, kind=c.tag)
                if c.tag == 'MidiClip':
                    notes, off = midi_notes(c)
                    row['session_clips'].append(dict(base, **clip_common(c), notes=notes, notes_disabled=off))
                elif c.tag == 'AudioClip':
                    row['session_clips'].append(dict(base, **audio_clip(c, als_dir)))
        out.append(row)
    return out


def audio_clips(live_set, als_dir, points):
    all_tracks = list(live_set.find('Tracks'))
    by_id = {int(t.get('Id')): t for t in all_tracks}
    out = []
    for tr in all_tracks:
        if tr.tag != 'AudioTrack':
            continue
        for c in tr.findall('DeviceChain/MainSequencer/Sample/ArrangerAutomation/Events/AudioClip'):
            start, end = val(c, 'CurrentStart', float), val(c, 'CurrentEnd', float)
            out.append(dict(audio_clip(c, als_dir), track=val(tr, 'Name/EffectiveName'),
                            groups=group_chain(by_id, val(tr, 'TrackGroupId', int)), start_beat=start, end_beat=end,
                            start_s=beat_seconds(points, start), end_s=beat_seconds(points, end)))
    return out


def grids(clips):
    out = {}
    for c in clips:
        m = c['markers']
        if c['warped'] and len(m) == 2 and c['file_path'] and c['file_path'] not in out:
            (s0, b0), (s1, b1) = m
            spb = (s1 - s0) / (b1 - b0)
            out[c['file_path']] = dict(file=c['file'], bpm=60 / spb, first_beat_s=s0 - b0 * spb)
    return out


def extract(als_path):
    als_path = Path(als_path)
    before = sha256(als_path)
    with gzip.open(als_path, 'rb') as f:
        live_set = ET.parse(f).getroot().find('LiveSet')
    pts = tempo_points(live_set)
    clips = audio_clips(live_set, als_path.parent, pts)
    trs = tracks(live_set, als_path.parent, pts)
    if sha256(als_path) != before:
        raise RuntimeError(f'{als_path} changed during the read; nothing written')
    return dict(source=dict(path=str(als_path), sha256=before),
                tempo_points=[dict(beat=b, bpm=v, recorded_at_seconds=beat_seconds(pts, b), source=SOURCE_ALS) for b, v in pts],
                audio_clips=clips, grids=grids(clips), tracks=trs)


def main():
    als = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path.home() / '.cache/manifold/ableton' / f'{als.stem}.json'
    res = extract(als)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(res, indent=1))
    trs = res['tracks']
    print(out, 'tempo points', len(res['tempo_points']), 'audio clips', len(res['audio_clips']), 'grids', len(res['grids']),
          'tracks', len(trs), 'midi notes played', sum(len(c['notes']) for t in trs for c in t['midi_clips']),
          'session clips', sum(len(t['session_clips']) for t in trs))


if __name__ == '__main__':
    main()
