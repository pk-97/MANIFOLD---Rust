"""Metadata-only inventory of the already identified local MANIFOLD audio roots.

No audio decoding, detector execution, spectral analysis, cloud hydration, or
archive extraction. Song identity and stem roles inferred from names are explicit
unverified hypotheses; existing registry entries retain their provenance.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import struct
import subprocess


ROOT = Path(__file__).resolve().parents[3]
MUSIC = Path.home() / 'Library/CloudStorage/Dropbox/Music Production'
FIXTURES = Path.home() / 'MANIFOLD - Rust/tests/fixtures/audio'
STEMS = MUSIC / 'Ableton Projects/STEMS'
MASTERS = MUSIC / 'EMERGENCE - ALBUM 2026/FINAL MASTERS'
REGISTRY = ROOT / 'tools/audio_analysis/eval/additional_stem_sources.json'
AUDIO_SUFFIXES = {'.wav', '.aif', '.aiff', '.mp3', '.flac', '.m4a'}
SF_DATALESS = 0x40000000  # Darwin st_flags: File Provider data is not resident.
DEVELOPMENT = ('apricots_128bpm', 'bad_guy_128bpm', 'feel_the_vibration_174bpm',
               'inhale_exhale_145bpm', 'tears_140bpm', 'late_night',
               'midnight_patience', 'miracle', 'heavy_on_mind')


def identity(value):
    return re.sub(r'[^a-z0-9]', '', value.lower())


def inferred_role(path):
    name = path.stem.lower()
    if any(x in name for x in ('no leads', 'no vox', 'no pianos', 'no vocal')):
        return 'alternate_mix'
    if name == 'mix':
        return 'mix'
    if re.search(r'\bkicks?\b', name):
        return 'kick'
    if 'drum' in name:
        return 'drums'
    if any(x in name for x in ('vox', 'vocal', 'adlib', 'adblib', 'choir', 'double')):
        return 'vocals'
    if any(x in name for x in ('bass', 'sub')):
        return 'bass_or_sub'
    if any(x in name for x in ('piano', 'chord', 'pad', 'string')):
        return 'harmonic_instrument'
    if any(x in name for x in ('lead', 'synth')):
        return 'lead_or_synth'
    if any(x in name for x in ('fx', 'reverb', 'delay', 'echo', 'rain', 'rev')):
        return 'effects'
    return 'unknown'


def metadata(path):
    info = path.stat()
    flags = getattr(info, 'st_flags', None)
    dataless = bool(flags & SF_DATALESS) if flags is not None else None
    return dict(path=str(path), bytes=info.st_size, allocated_bytes=info.st_blocks * 512,
                st_flags=flags, online_only=dataless,
                availability_evidence='Darwin SF_DATALESS metadata; no file-content availability probe',
                inferred_role=inferred_role(path), role_verified=False)


def wav_header(path):
    """Read only RIFF chunk headers and fmt; never read or decode sample bytes."""
    with path.open('rb') as handle:
        header = handle.read(12)
        if len(header) != 12 or header[:4] != b'RIFF' or header[8:] != b'WAVE':
            return dict(status='unsupported_header; no audio decoded')
        fmt = None
        for _ in range(128):
            chunk = handle.read(8)
            if len(chunk) != 8:
                break
            kind, size = struct.unpack('<4sI', chunk)
            if kind == b'fmt ':
                data = handle.read(min(size, 64))
                if len(data) < 16:
                    raise ValueError(f'truncated fmt header: {path}')
                encoding, channels, sr, byte_rate, align, bits = struct.unpack('<HHIIHH', data[:16])
                fmt = dict(format_code=encoding, channels=channels, sample_rate=sr,
                           block_align=align, bits_per_sample=bits)
                handle.seek(size - len(data) + size % 2, 1)
            elif kind == b'data' and fmt:
                frames = size // fmt['block_align']
                return dict(status='resident_RIFF_header_only', **fmt, frames=frames,
                            duration_s=frames / fmt['sample_rate'])
            else:
                handle.seek(size + size % 2, 1)
    return dict(status='no_supported_data_header; no audio decoded')


def usage_audit():
    paths = subprocess.check_output(
        ['git', 'ls-files', 'tools/audio_analysis/eval/*.json'], cwd=ROOT, text=True).splitlines()
    identifiers, provenance = {}, []
    def walk(value, path, location='$'):
        if isinstance(value, dict):
            for key, item in value.items():
                if key in ('id', 'track', 'track_id', 'track_name', 'song', 'name') and isinstance(item, str):
                    identifiers.setdefault(identity(item), []).append(dict(
                        identifier=item, file=path, json_location=f'{location}.{key}'))
                walk(item, path, f'{location}.{key}')
        elif isinstance(value, list):
            for index, item in enumerate(value):
                walk(item, path, f'{location}[{index}]')
    for relative in paths:
        # The source registry contains obsolete reserve labels, not detector use.
        if relative.endswith('additional_stem_sources.json'):
            continue
        raw = (ROOT / relative).read_bytes()
        walk(json.loads(raw), relative)
        provenance.append(dict(path=relative, sha256=hashlib.sha256(raw).hexdigest()))
    return identifiers, provenance


def prior_use(family, identifiers):
    matches = identifiers.get(identity(family), []) + identifiers.get(identity('liveshow_' + family), [])
    # Keep one precise occurrence per file; repeated per-parameter rows do not
    # imply independent recordings or independent evaluation sets.
    unique = {m['file']: m for m in matches}
    scored = [m for p, m in unique.items() if '/scoreboard/' in p]
    return dict(status='previously_evaluated_development' if scored or family in DEVELOPMENT
                else 'no_prior_evaluation_found_in_audited_committed_records',
                detector_record_evidence=sorted(scored, key=lambda x: x['file']),
                other_metadata_evidence=[m for p, m in sorted(unique.items()) if '/scoreboard/' not in p],
                qualification='Absence in committed records is not proof against unrecorded use; '
                'all versions of a known evaluated song stay development.')


def run(out):
    registry = json.loads(REGISTRY.read_text())
    identifiers, audited = usage_audit()
    registry_files = {}
    for index, group in enumerate(registry['sets']):
        for file_index, row in enumerate(group['files']):
            registry_files[str(Path(registry['source_root']) / row['file'])] = dict(
                registry='tools/audio_analysis/eval/additional_stem_sources.json',
                json_pointer=f'/sets/{index}/files/{file_index}', duration_s=row['duration_s'],
                sample_rate=int(row['audio']['sample_rate']), hash_reference='sha256 in referenced registry row')
        master = group.get('master', {})
        if 'path' in master:
            registry_files[master['path']] = dict(
                registry='tools/audio_analysis/eval/additional_stem_sources.json',
                json_pointer=f'/sets/{index}/master', duration_s=master['duration_s'],
                sample_rate=int(master['audio']['sample_rate']), hash_reference='sha256 in referenced registry row')
    header_reads = []
    def file_row(path, read_header=False):
        row = metadata(path)
        reference = registry_files.get(str(path))
        if reference:
            row['existing_registry_metadata'] = reference
        elif read_header and path.suffix.lower() == '.wav' and row['online_only'] is False:
            row['header_metadata'] = wav_header(path)
            header_reads.append(str(path))
        return row
    fixtures = []
    for folder in sorted(FIXTURES.iterdir()):
        if not folder.is_dir():
            continue
        audio = [p for p in sorted(folder.rglob('*')) if p.is_file() and p.suffix.lower() in AUDIO_SUFFIXES]
        fixtures.append(dict(folder=str(folder), family=folder.name,
            kind='synthetic_renders' if folder.name == 'renders' else 'fixture_song',
            prior_use=prior_use(folder.name, identifiers),
            files=[file_row(p, p.name == 'mix.wav') for p in audio]))
    stem_groups = []
    for folder in sorted(STEMS.iterdir()):
        if not folder.is_dir():
            continue
        family = re.sub(r'\s+STEMS$', '', folder.name, flags=re.I).lower().replace(' ', '_')
        if folder.name == 'Miracle Stripped (wet stems)':
            family = 'miracle'
        direct = [p for p in sorted(folder.iterdir()) if p.is_file() and p.suffix.lower() in AUDIO_SUFFIXES]
        nested = [p for p in sorted(folder.rglob('*')) if p.is_file()
                  and p.parent != folder and p.suffix.lower() in AUDIO_SUFFIXES]
        # One local representative per unregistered song set; not acapellas or
        # project fragments. Other durations remain explicitly unverified.
        representative = next((p for p in direct if not metadata(p)['online_only']), None)
        stem_groups.append(dict(folder=str(folder), family=family,
            kind='auxiliary_acapellas' if family == 'acapellas' else 'song_stem_folder',
            prior_use=prior_use(family, identifiers),
            direct_audio=[file_row(p, p == representative and family != 'acapellas') for p in direct],
            nested_audio=[file_row(p) for p in nested],
            nested_qualification='Project edits/fragments and dry variants are metadata-only inventory, '
                                 'not additional independent song coverage.'))
    masters = []
    for path in sorted(MASTERS.iterdir()):
        if not path.is_file() or path.suffix.lower() not in AUDIO_SUFFIXES:
            continue
        match = re.match(r'Latent Space - \d+\s*-?\s*(.*?)\s+(?:T\d|MSTR)', path.stem)
        if not match:
            raise ValueError(f'unrecognised master naming: {path.name}')
        family = match[1].lower().replace("'", '').replace(' ', '_')
        row = file_row(path, True)
        row.update(family=family, inferred_role='finished_master', prior_use=prior_use(family, identifiers))
        matches = [s['folder'] for s in stem_groups if identity(s['family']) == identity(family)]
        row['matching_stem_folders'] = matches
        row['match_evidence'] = ('Existing registry identifies both source set and master; consult its '
            'alignment qualification.' if str(path) in registry_files else
            'Song-title identity from filenames only; no alignment or mix-equivalence claim.')
        masters.append(row)
    identified_other_masters = []
    for group in registry['sets']:
        path = Path(group['master']['path'])
        if path.parent != MASTERS:
            row = file_row(path)
            family = re.sub(r'\s+STEMS$', '', group['folder']).lower().replace(' ', '_')
            row.update(family=family, prior_use=prior_use(family, identifiers),
                       inferred_role='finished_master', match_evidence='Existing registry association')
            identified_other_masters.append(row)
    archives = [metadata(p) for p in sorted(STEMS.iterdir()) if p.is_file() and p.suffix.lower() == '.zip']
    result = dict(method=__doc__, date='2026-10-09',
        roots=dict(fixtures=str(FIXTURES), stems=str(STEMS), final_masters=str(MASTERS)),
        existing_registry='tools/audio_analysis/eval/additional_stem_sources.json',
        existing_nine_all_development=list(DEVELOPMENT),
        usage_audit=dict(record_count=len(audited), committed_records=audited,
            method='Exact normalised identity in id/track/track_id/track_name/song/name fields, '
                   'including liveshow aliases. No substring genre/instrument inference.'),
        fixtures=fixtures, stem_folders=stem_groups, final_masters=masters,
        additional_identified_masters=identified_other_masters,
        unopened_archives=archives, resident_header_reads=header_reads,
        proposed_split=dict(status='lead_approved_whole_family_reserve_2026-10-09',
            whole_song_reserve_candidates=['waypoints', 'know_youre_there'],
            locked_reserve_families=['waypoints', 'know_youre_there'],
            reserve_rule='No audio samples, features or labels from any version of either family '
            'until the lead freezes the candidate for final validation.',
            reserve_reason='Finished resident masters and matching named stem folders; no detector '
            'use found in audited committed records. Lead approved this whole-family reserve.',
            immediate_expansion_priority=['late_night', 'midnight_patience', 'miracle', 'heavy_on_mind'],
            additional_development_priority=['pattern', 'all_in_for_you', 'integer'],
            development_reason='Already exposed in earlier liveshow detector evaluations; cannot claim '
            'untouched whole-song validation. Pattern has drums and premaster reference; All In For '
            'You has a finished master; Integer has a resident no-leads mix.',
            other_available_song_families=['imitation', 'dimension', 'burn', 'touch', 'break',
                                           'waiting_for_you', 'states', 'staged', 'weight'],
            optional_new_development='Weight has resident named drums/other stems; States a resident '
            'alternate mix. Staged offers named stems/alternate mixes currently dataless. Remaining '
            'album masters are resident; no matching stem sets found in the authorised roots.'),
        coverage_limitations=[
            'No genre, section type, kick presence, stem purity or source alignment is inferred from filenames.',
            'New matched stem sets lack a separately named isolated kick stem: reliable annotation still needs review.',
            'No reserved audio samples inspected, no decoding/features/detector/labels/scores, and no files hydrated.',
            'Known evaluated Miracle includes stripped/wet variants; old registry held_out labels are superseded.',
            'Online-only status is a point-in-time filesystem flag; resident status is not complete-file integrity proof.',
            'Nested project fragments and source archives are not counted as new independent tracks.',
        ])
    resident_masters = [m for m in masters + identified_other_masters if m['online_only'] is False]
    duration = lambda m: (m.get('header_metadata') or m['existing_registry_metadata'])['duration_s']
    result['resident_finished_master_duration_s'] = sum(map(duration, resident_masters))
    result['locked_reserve_master_duration_s'] = sum(
        duration(m) for m in resident_masters
        if m['family'] in result['proposed_split']['locked_reserve_families'])
    out.write_text(json.dumps(result, indent=2) + '\n')
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    result = run(args.out)
    print(json.dumps(dict(stem_folders=len(result['stem_folders']),
                          final_masters=len(result['final_masters']),
                          resident_header_reads=len(result['resident_header_reads']),
                          audited_records=result['usage_audit']['record_count'])))
