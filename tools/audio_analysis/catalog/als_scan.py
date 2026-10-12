#!/usr/bin/env python3
"""Streaming reader of an Ableton set (.als, gzipped XML) for the catalogue: which tracks carry kick, snare/clap, hat
and percussion MIDI, which drum tracks are audio clips, what the tempo and arrangement length are. It answers "could
the existing label code (eval/detector_labels.project_parts, eval/snare_labels) turn this set into labels?" without
building the whole document: each track is parsed, summarised and dropped, so a 50 MB set costs one track of memory.

Reused from eval/als_extract (the label code's reader): val, devices (drum-rack pads with their MIDI keys), midi_notes,
clip_common, unroll (loops applied), file_path, tempo_points, beat_seconds. Reused from the label code: the snare/clap
voice pattern (snare_labels.FAMILY) and the part classes (part_classes.classify). A set is only ever opened for reading.

Rules, mirroring project_parts: a track sounds unless its speaker, or a group above it, is off (note_labels.silent);
notes with probability under 1 are unknown; the family's pads are found by pad name, else the track's name.
A set is label-ready (`ready`) when sounding MIDI gives MIN_NOTES or more kick notes or snare/clap notes; `audio_only`
when the drums are audio clips (different handling: one-shots or bounces on drum tracks); `muted_only` when the only
drum MIDI sits on muted tracks (unknown, not silent); `none` otherwise.

Usage: als_scan.py FILE.als      prints the summary"""
import gzip
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from tools.audio_analysis.eval import als_extract as AE  # noqa: E402
from tools.audio_analysis.eval.part_classes import classify as part_class  # noqa: E402
from tools.audio_analysis.eval.snare_labels import FAMILY as SNARE_FAMILY  # noqa: E402

MIN_NOTES = 16
KICK = re.compile(r'kick|kik\b|\bbd\b|bass ?drum', re.I)
HAT = re.compile(r'hat|\bhh|ride|cymbal|crash|shaker|shkr|tamb', re.I)
PERC = re.compile(r'perc|tom|conga|bongo|cowbell|clave|wood|block|triangle|guiro|cabasa|snap|rim|xstick|cross', re.I)
TRACKS = ('MidiTrack', 'AudioTrack', 'GroupTrack', 'ReturnTrack')


def voice_of(name):
    """kick | snare | hat | perc | other for a track or pad name. Snare includes clap (one class in the labels); rim and
    cross-stick are percussion (the snare family excludes them)."""
    name = re.sub(r'^\d+[- ]', '', name or '')
    if SNARE_FAMILY.exclude.search(name):
        return 'perc'
    if KICK.search(name):
        return 'kick'
    if SNARE_FAMILY.voice.search(name):
        return 'snare'
    if HAT.search(name):
        return 'hat'
    if PERC.search(name):
        return 'perc'
    return 'other'


def _pads(devs, out):
    for d in devs or []:
        for b in d.get('branches') or []:
            if b.get('key') is not None:
                out[b['key']] = (b.get('name') or '', b.get('on', True))
            _pads(b.get('devices'), out)


def rack_pads(devices):
    """{midi key: (pad name, on)} for every drum-rack pad in a device tree."""
    out = {}
    _pads(devices, out)
    return out


def summarise_midi(clips_xml, pads, track_name, als_dir):
    """Counts of played notes by voice (probability 1), unknown (probability under 1), and the arrangement span in beats."""
    by_voice, unknown, per_pad = {}, 0, {}
    first, last, n_clips, n_off = None, None, 0, 0
    track_voice = voice_of(track_name)
    for c in clips_xml:
        clip = AE.clip_common(c)
        start, end = AE.val(c, 'CurrentStart', float), AE.val(c, 'CurrentEnd', float)
        n_clips += 1
        first = start if first is None else min(first, start)
        last = end if last is None else max(last, end)
        if clip['disabled']:
            n_off += 1
            continue
        notes, _ = AE.midi_notes(c)
        for beat, n in AE.unroll(clip, notes, start, end):
            if n['probability'] < 1:
                unknown += 1
                continue
            if n['key'] in pads:
                name, on = pads[n['key']]
                v = voice_of(name)
                if v == 'other' and track_voice != 'other':
                    v = track_voice
                per_pad[n['key']] = per_pad.get(n['key'], 0) + 1
            else:
                v = track_voice
            by_voice[v] = by_voice.get(v, 0) + 1
    pad_rows = [dict(key=k, name=pads[k][0], on=pads[k][1], notes=per_pad.get(k, 0)) for k in sorted(pads)]
    return dict(clips=n_clips, disabled=n_off, by_voice=by_voice, unknown=unknown, first_beat=first, last_beat=last,
                pads=[p for p in pad_rows if p['notes']], pad_count=len(pads))


def summarise_track(tr, als_dir):
    """One track element -> a plain dict. Only arrangement clips count as played; session clips are counted."""
    dev = AE.devices(tr.find('DeviceChain/DeviceChain/Devices'), als_dir)
    name = AE.val(tr, 'Name/EffectiveName') or ''
    row = dict(id=int(tr.get('Id')), kind=tr.tag, name=name, group_id=AE.val(tr, 'TrackGroupId', int),
               speaker_on=AE.val(tr, 'DeviceChain/Mixer/Speaker/Manual') != 'false', frozen=AE.val(tr, 'Freeze') == 'true',
               part=part_class(name), rack=False, devices=[d['name'] for d in dev][:6])
    if tr.tag == 'MidiTrack':
        pads = rack_pads(dev)
        row['rack'] = bool(pads)
        clips = tr.findall('DeviceChain/MainSequencer/ClipTimeable/ArrangerAutomation/Events/MidiClip')
        row['midi'] = summarise_midi(clips, pads, name, als_dir)
        row['session_midi_clips'] = len(tr.findall('DeviceChain/MainSequencer/ClipSlotList/ClipSlot/ClipSlot/Value/MidiClip'))
    elif tr.tag == 'AudioTrack':
        clips = tr.findall('DeviceChain/MainSequencer/Sample/ArrangerAutomation/Events/AudioClip')
        files, off, end = [], 0, None
        for c in clips:
            cc = AE.clip_common(c)
            off += cc['disabled']
            e = AE.val(c, 'CurrentEnd', float)
            end = e if end is None else max(end, e)
            f = AE.file_path(c.find('SampleRef/FileRef'), als_dir)
            if f and Path(f).name not in files and len(files) < 6:
                files.append(Path(f).name)
        row['audio'] = dict(clips=len(clips), disabled=off, files=files, last_beat=end)
        row['session_audio_clips'] = len(tr.findall('DeviceChain/MainSequencer/ClipSlotList/ClipSlot/ClipSlot/Value/AudioClip'))
    return row


def scan(path):
    """The summary of one set: creator, tempo, arrangement length, tracks (muted resolved), and the readiness verdict."""
    path = Path(path)
    stack, tracks, main = [], [], None
    creator = None
    with gzip.open(path, 'rb') as f:
        for ev, el in ET.iterparse(f, events=('start', 'end')):
            if ev == 'start':
                stack.append(el.tag)
                if len(stack) == 1:
                    creator = el.get('Creator')
                continue
            stack.pop()
            if stack == ['Ableton', 'LiveSet', 'Tracks'] and el.tag in TRACKS:
                tracks.append(summarise_track(el, path.parent))
                el.clear()
            elif stack == ['Ableton', 'LiveSet'] and el.tag in ('MainTrack', 'MasterTrack'):
                main = el
            elif stack == ['Ableton', 'LiveSet'] and el.tag not in ('Tracks',):
                el.clear()
    if main is None:
        raise ValueError('no main track')
    live = ET.Element('LiveSet')
    live.append(main)
    pts = AE.tempo_points(live)
    resolve(tracks, pts)
    bpms = [b for _, b in pts]
    end_beat = max([t['midi']['last_beat'] for t in tracks if t.get('midi') and t['midi']['last_beat'] is not None] +
                   [t['audio']['last_beat'] for t in tracks if t.get('audio') and t['audio']['last_beat'] is not None] or [0.0])
    out = dict(creator=creator, tempo=round(pts[0][1], 3), tempo_min=round(min(bpms), 3), tempo_max=round(max(bpms), 3),
               tempo_points=len(pts), arrangement_beats=round(end_beat, 2), arrangement_s=round(AE.beat_seconds(pts, end_beat), 1),
               tracks=tracks)
    out['ready'] = readiness(tracks)
    return out


def resolve(tracks, pts):
    """Fills each track's groups (names), muted flag (own speaker or any group above off) and its note span in seconds."""
    by_id = {t['id']: t for t in tracks}
    for t in tracks:
        names, off, g, seen = [], not t['speaker_on'], t['group_id'], set()
        while g not in (None, -1) and g in by_id and g not in seen:
            seen.add(g)
            names.append(by_id[g]['name'])
            off = off or not by_id[g]['speaker_on']
            g = by_id[g]['group_id']
        t['groups'], t['muted'] = names, off
        m = t.get('midi')
        if m and m['first_beat'] is not None:
            m['first_s'], m['last_s'] = round(AE.beat_seconds(pts, m['first_beat']), 1), round(AE.beat_seconds(pts, m['last_beat']), 1)
        t['part'] = part_class(t['name'], t['groups'])


def readiness(tracks):
    """Counts of sounding and muted kick/snare/hat/perc MIDI notes, the audio drum tracks, and the verdict."""
    play, muted = {}, {}
    names = {'kick': [], 'snare': []}
    audio = []
    for t in tracks:
        m = t.get('midi')
        if m:
            for v, n in m['by_voice'].items():
                d = muted if t['muted'] else play
                d[v] = d.get(v, 0) + n
                if v in names and n and not t['muted']:
                    names[v].append(f"{t['name']} ({n})")
        a = t.get('audio')
        if a and a['clips'] > a['disabled'] and not t['muted'] and (voice_of(t['name']) != 'other' or t['part'] == 'drums'):
            audio.append(dict(track=t['name'], clips=a['clips'] - a['disabled'], files=a['files'], voice=voice_of(t['name']),
                              frozen=t['frozen']))
    live = max(play.get('kick', 0), play.get('snare', 0))
    hid = max(muted.get('kick', 0), muted.get('snare', 0))
    if live >= MIN_NOTES:
        verdict = 'ready'
    elif audio:
        verdict = 'audio_only'
    elif hid >= MIN_NOTES:
        verdict = 'muted_only'
    else:
        verdict = 'none'
    return dict(verdict=verdict, kick_notes=play.get('kick', 0), snare_notes=play.get('snare', 0), hat_notes=play.get('hat', 0),
                perc_notes=play.get('perc', 0), muted_kick=muted.get('kick', 0), muted_snare=muted.get('snare', 0),
                kick_tracks=names['kick'], snare_tracks=names['snare'], audio_drums=audio,
                audio_drums_with_midi=bool(audio) and live >= MIN_NOTES)


def main():
    r = scan(sys.argv[1])
    for t in r['tracks']:
        t.pop('devices', None)
    print(json.dumps(r, indent=1, default=str))


if __name__ == '__main__':
    main()
