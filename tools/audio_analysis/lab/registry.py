#!/usr/bin/env python3
"""The lab's song list (docs/DETECTOR_LAB_DESIGN.md D2): every song a family labels, the audio file the legacy
loader reads for it, that file's sha256, its export offset and the offset's proof. Built from the family labels files
under the pinned recipe (lab/recipe.py), so which songs exist never depends on the caller's environment. songs.json
is rebuilt, never hand-edited, except audit_override, which only Peter sets and a rebuild keeps.

Usage: registry.py build | show [SONG] | verify [SONG...]"""
import functools
import hashlib
import json
import os
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

SONGS = Path(__file__).with_name('songs.json')
HASHES = Path.home() / '.cache/manifold/detector-lab/source_hashes.json'   # path -> [size, mtime_ns, sha256]
# Family -> its labels files under GOAL (project songs, then generated).
FAMILIES = {'snare': ('snare_labels.json', 'snare_labels_generated.json'),
            'bass': ('bass_labels.json', 'bass_labels_generated.json'),
            'synth': ('synth_labels.json', 'synth_labels_generated.json'),
            'kick': ('kick_labels.json', 'kick_labels_project.json', 'kick_labels_generated.json')}
GEN_SR = 48000


@dataclass(frozen=True)
class Song:
    id: str
    kind: str                     # 'project' | 'generated'
    source: dict                  # {'how': 'goal'|'wip'|'section', 'paths': [...], 'start_s'?, 'end_s'?}
    source_sha256: tuple
    sample_rate: int
    offset_s: float | None
    proof: str                    # 'wip:...' | 'goal:...' | 'generated' | 'none'
    families: tuple
    audit_override: str | None = None
    gain_db: float | None = None   # a gain copy: the parent's audio scaled by this (gain_variants)
    parent: str | None = None      # the song a gain copy was made from
    probe: bool = False            # a gain copy that is only predicted, never trained on (gain_probes)

    @property
    def content_id(self):
        """What the song's audio is: its source bytes, section bounds and rate. Caches key on this, never on id."""
        key = dict(sha=list(self.source_sha256), start=self.source.get('start_s'), end=self.source.get('end_s'),
                   sr=self.sample_rate, how=self.source['how'])
        if self.gain_db is not None:
            key['gain_db'] = self.gain_db
        return hashlib.sha256(json.dumps(key, sort_keys=True).encode()).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def check_sources(songs):
    """Exits unless every song's source files still hash to what songs.json recorded. A file is re-hashed only when
    its size or modified time moved, so the check costs a stat per file on a quiet day."""
    memo = json.loads(HASHES.read_text()) if HASHES.exists() else {}
    bad, dirty = [], False
    for s in songs:
        for path, want in zip(s.source['paths'], s.source_sha256):
            st = os.stat(path)
            stamp = [st.st_size, st.st_mtime_ns]
            if memo.get(path, [None, None])[:2] != stamp:
                memo[path], dirty = stamp + [sha256_file(path)], True
            if memo[path][2] != want:
                bad.append(f'{s.id}: {path}')
    if dirty:
        HASHES.parent.mkdir(parents=True, exist_ok=True)
        tmp = HASHES.with_suffix('.tmp')
        tmp.write_text(json.dumps(memo))
        tmp.replace(HASHES)
    if bad:
        sys.exit('source audio changed since the song list was built; run lab/registry.py build, then audit:\n'
                 + '\n'.join(sorted(set(bad))))


def load():
    rows = json.loads(SONGS.read_text())
    return {r['id']: Song(**{**r, 'source_sha256': tuple(r['source_sha256']), 'families': tuple(r['families'])})
            for r in rows}


@functools.lru_cache(maxsize=2)
def _export(path):
    from tools.audio_analysis.eval.detector_labels import read_any
    from tools.audio_analysis.eval.kick_goal_eval import load as load_audio
    return read_any(path, GEN_SR, load_audio)


def mix(song):
    """Mono float64 audio, byte-identical to the legacy loader (detector_songs.audio)."""
    from tools.audio_analysis.eval.kick_attack_rejection import read_audio
    path = song.source['paths'][0]
    if song.source['how'] == 'section':
        x = _export(path)
        a, b = int(song.source['start_s'] * GEN_SR), int(song.source['end_s'] * GEN_SR)
        return x[a:b].astype(np.float32).astype(np.float64)
    if path.endswith('.npy'):
        return np.load(path).astype(np.float64)
    return read_audio(path, song.sample_rate)[1]


def gain_gains(song_id, count, lo, hi):
    """The fixed gains (dB) of a song's copies: distinct draws, uniform on a 0.1 dB grid (never 0), seeded by the
    song's id alone, so they never depend on the training seed, the config's other settings or the other songs."""
    import zlib
    grid = np.round(np.arange(lo, hi + 1e-9, .1), 1)
    rng = np.random.default_rng(zlib.crc32(song_id.encode()))
    return [float(g) for g in rng.choice(grid[grid != 0], count, replace=False)]


def gain_copy(song, db, probe=False):
    """The song's copy at a gain: the same source scaled. Every artifact is computed from the scaled audio (mix),
    cached under the copy's own content id. A probe is held out and predicted (level-robustness scoring), never
    trained on."""
    import dataclasses
    return dataclasses.replace(song, id=f'{song.id}@{"probe" if probe else ""}{db:+.1f}dB', gain_db=float(db),
                               parent=song.id, probe=probe)


def gain_copies(song, count, lo, hi):
    """The song's training copies at fixed seeded gains."""
    return [gain_copy(song, g) for g in gain_gains(song.id, count, lo, hi)]


# Mix and master variants (sampling.mix_variants): another bounce of a project song that the catalogue judged
# same_arrangement against the lab mix, with its own export offset proven by lab/offsets.py --variants. A variant is a
# training copy of its song like a gain copy: its own audio and content id, the song's labels moved onto its timeline.
VARIANT_PROOFS = Path(__file__).with_name('variant_proofs.json')
# Label fields that hold times (s) on the mix timeline, as points or [start, end] spans; every other field is a
# setting. A field in neither list is refused, so a new time field can never ride along unshifted.
LABEL_TIMES = ('positives', 'hard_neg', 'dropped', 'dense_pos', 'dense_all')
LABEL_SPANS = ('unscored', 'loop_spans', 'dense')
LABEL_SETTINGS = ('kind', 'positives_only', 'timing_shift_ms', 'unmuted', 'drum_stem', 'sources', 'builder', 'proof',
                  'counts', 'kinds', 'window_s', 'tol_s')


def variant_id(parent_id, sha):
    return f'{parent_id}@mix-{sha[:10]}'


def variant_copy(song, rec):
    """A proven variant (a row of variant_proofs.json) of song as a training copy. Its audio is the variant file read
    at the song's rate; its offset is the one its labels use, which lab/offsets.py proved on its own audio."""
    import dataclasses
    return dataclasses.replace(song, id=variant_id(song.id, rec['sha256']), source=dict(how='variant', paths=[rec['path']]),
                               source_sha256=(rec['sha256'],), offset_s=rec['offset_s'], proof=f"lab:{rec['status']}",
                               audit_override=None, gain_db=None, parent=song.id, probe=False)


def is_variant(song):
    return song.source['how'] == 'variant'


def variant_shift(song, parent):
    """Seconds a variant's timeline runs behind its song's: the difference of their proven export offsets."""
    return song.offset_s - parent.offset_s


def variant_labels(labels, shift_s, duration_s):
    """The song's labels on a variant's timeline: every time moved by shift_s, the duration the variant's."""
    out = {}
    for k, v in labels.items():
        if k in LABEL_TIMES:
            out[k] = [t + shift_s for t in v]
        elif k in LABEL_SPANS:
            out[k] = [[a + shift_s, b + shift_s] for a, b in v]
        elif k == 'duration_s':
            out[k] = duration_s
        elif k == 'offset_s':
            out[k] = None if v is None else v + shift_s
        elif k in LABEL_SETTINGS:
            out[k] = v
        else:
            raise ValueError(f'label field {k!r} is neither a time nor a setting: add it to registry.LABEL_* first')
    return out


def variant_proofs():
    """{parent song: [proof rows]}: every variant lab/offsets.py --variants judged, proven or not."""
    rows = json.loads(VARIANT_PROOFS.read_text()) if VARIANT_PROOFS.exists() else {}
    out = {}
    for vid, r in sorted(rows.items()):
        out.setdefault(r['parent'], []).append(dict(r, id=vid))
    return out


def variants_of(song, proofs):
    """([(training copy, its proof row)], {variant id: why it is not one}) for one song from variant_proofs()."""
    used, excluded = [], {}
    for r in proofs.get(song.id, ()):
        if r['status'].startswith('proven'):
            used.append((variant_copy(song, r), r))
        else:
            excluded[r['id']] = r['status']
    return used, excluded


# The release holdout: about one project song in six, picked by a salted hash of its id alone, so the slice never
# moves when songs are added or reordered. Lab runs never score these songs unless invoked with --release.
HOLDOUT_SALT = 'manifold-detector-release-holdout-1'
HOLDOUT_ONE_IN = 6


def in_holdout(song_id):
    return int(hashlib.sha256(f'{HOLDOUT_SALT}:{song_id}'.encode()).hexdigest(), 16) % HOLDOUT_ONE_IN == 0


def release_holdout(songs):
    """The holdout songs among songs ({id: Song}): project songs only, in the given order."""
    return [t for t, s in songs.items() if s.kind == 'project' and s.parent is None and in_holdout(t)]


# Wrapping below the original mix() body. Reader note: the wraps chain must still unwrap to that byte-identical
# body (cache.loader_id()/code_id(mix) walk __wrapped__, lab/cache.py:43-44) and the body's referenced globals
# (_export, read_audio) are untouched, so no artifact stamp moves; only the decode is memoized. The cache is
# bounded (maxsize=3: a song and its gain copies build consecutively, so three decoded songs cover a build's
# active window) and never stores a gain-scaled array - a copy scales a fresh one inside _with_gain.
# A Song carries dict fields (an lru_cache cannot key on it), so the key is the hashable identity the body reads.
_mix_body = mix


def _decode_key(song):
    """The hashable identity of the decode the body above performs: a song and its gain copies share one key."""
    return (song.source['how'], tuple(song.source['paths']), song.source.get('start_s'), song.source.get('end_s'),
            song.sample_rate)


@functools.lru_cache(maxsize=3)
def _decoded(key):
    """The body's decoded parent-scale audio, one bounded entry per process."""
    from types import SimpleNamespace
    how, paths, start_s, end_s, sample_rate = key
    if how == 'variant' and not paths[0].lower().endswith('.wav'):
        # A variant is any bounce the catalogue aligned (mp3, aiff, flac): decoded by the catalogue's own reader, so
        # its measured lag (decoder delay included) holds. Its content id is its file's bytes, so no cached stamp moves.
        from tools.audio_analysis.lab import remix
        return remix.read_mono(paths[0], sample_rate)
    return _mix_body(SimpleNamespace(source=dict(how=how, paths=list(paths), start_s=start_s, end_s=end_s),
                                      sample_rate=sample_rate))


@functools.wraps(mix)
def _mix_memo(song):
    """mix() through the decode memo. functools.wraps keeps this wrapper's __wrapped__ at the original body, so
    code_id(mix) keeps hashing that body and every cached artifact stamp stays valid."""
    return _decoded(_decode_key(song))


def _with_gain(fn):
    """mix() for a gain copy scales the parent's audio. functools.wraps keeps code_id(mix) as it was, so the cached
    artifacts of every ordinary song stay valid; a copy's content id carries its gain."""
    @functools.wraps(fn)
    def wrapper(song):
        x = fn(song)
        return x if song.gain_db is None else x * 10.0 ** (song.gain_db / 20.0)
    return wrapper


mix = _with_gain(_mix_memo)


def _proof(t, W):
    from tools.audio_analysis.eval.kick_goal_melodic import PROJECT, SURVEY, TRIGGER
    from tools.audio_analysis.lab.offsets import PROOFS
    if t in W:
        return f"wip:{W[t]['status']}"
    lab = json.loads(PROOFS.read_text()).get(t) if PROOFS.exists() else None
    if lab and lab['status'].startswith('proven'):
        return f"lab:{lab['status']}"
    if t in TRIGGER:
        return 'goal:trigger track export offset (labels_trigger.json)'
    if (SURVEY / f'{t}.json').exists():
        return 'goal:wip survey offset (unproven against a stem)'
    if t in PROJECT:
        return 'goal:kick stem against project kick notes'
    return 'none'


def build():
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    g = Goal(mode='v3')
    W = wip()
    old = {s.id: s for s in load().values()} if SONGS.exists() else {}
    fams, rows = {}, {}
    for fam, files in FAMILIES.items():
        for f in files:
            for t, s in json.loads((GOAL / f).read_text()).items():
                fams.setdefault(t, []).append(fam)
                if t in rows:
                    if s.get('kind') == 'generated' and rows[t]['source'].get('paths') != [s['export']]:
                        sys.exit(f'{t}: two families label different audio under one song id')
                    continue
                if s.get('kind') == 'generated':
                    src = dict(how='section', paths=[s['export']], start_s=s['section'][0], end_s=s['section'][1])
                    rows[t] = dict(kind='generated', source=src, sample_rate=GEN_SR, offset_s=s['offset_s'], proof='generated')
                    continue
                if t in g.records:
                    info = g.labels['new'].get(t)
                    path = (info['mix_path'] or str(GOAL / f'{t}_mix.npy')) if info else g.records[t]['source']['audio_path']
                    src, sr = dict(how='goal', paths=[str(path)]), int(g.records[t]['sample_rate'])
                else:
                    src, sr = dict(how='wip', paths=[str(GOAL / 'snare_wip' / f'{t}.npy')]), 48000
                off = W[t]['offset_s'] if t in W else (_source(t) or (None, None))[1]
                rows[t] = dict(kind='project', source=src, sample_rate=sr, offset_s=off, proof=_proof(t, W))
    out = []
    for t, r in rows.items():
        sha = tuple(sha256_file(p) for p in r['source']['paths'])
        prev = old.get(t)
        out.append(asdict(Song(id=t, source_sha256=sha, families=tuple(fams[t]),
                               audit_override=prev.audit_override if prev else None, **r)))
    SONGS.write_text(json.dumps(out, indent=1) + '\n')
    n = {k: sum(r['kind'] == k for r in out) for k in ('project', 'generated')}
    print(f"{len(out)} songs ({n['project']} project, {n['generated']} generated) -> {SONGS}")


def verify(ids=()):
    """Each song's recorded source hash still matches, and mix() equals the legacy loader's audio exactly."""
    from tools.audio_analysis.eval.detector_songs import audio
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    g = Goal(mode='v3')
    bad = 0
    for t, s in load().items():
        if ids and t not in ids:
            continue
        sha_ok = tuple(sha256_file(p) for p in s.source['paths']) == s.source_sha256
        a, b = mix(s), audio(g, t)
        diff = float(np.max(np.abs(a - b))) if len(a) == len(b) else float('inf')
        ok = sha_ok and diff == 0
        bad += not ok
        print(f"{t:18s} {s.kind:9s} source hash {'ok' if sha_ok else 'CHANGED'}  max abs diff {diff:g}  {'ok' if ok else 'FAIL'}")
    print(f'{bad} failing')
    return bad


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else 'show'
    if cmd == 'build':
        build()
    elif cmd == 'verify':
        sys.exit(1 if verify(set(sys.argv[2:])) else 0)
    elif cmd == 'show':
        for t, s in load().items():
            if len(sys.argv) > 2 and t != sys.argv[2]:
                continue
            print(f"{t:18s} {s.kind:9s} {s.source['how']:7s} sr {s.sample_rate}  offset {s.offset_s}  proof {s.proof}"
                  f"{'  OVERRIDE: ' + s.audit_override if s.audit_override else ''}")
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main()
