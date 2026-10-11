#!/usr/bin/env python3
"""The lab's song list (docs/DETECTOR_LAB_DESIGN.md D2): every song a family labels, the audio file the legacy
loader reads for it, that file's sha256, its export offset and the offset's proof. Built from the family labels files
under the pinned recipe (lab/recipe.py), so which songs exist never depends on the caller's environment. songs.json
is rebuilt, never hand-edited, except audit_override, which only Peter sets and a rebuild keeps.

Usage: registry.py build | show [SONG] | verify [SONG...]"""
import functools
import hashlib
import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

SONGS = Path(__file__).with_name('songs.json')
# Family -> its labels files under GOAL (project songs, then generated).
FAMILIES = {'snare': ('snare_labels.json', 'snare_labels_generated.json'),
            'bass': ('bass_labels.json', 'bass_labels_generated.json'),
            'synth': ('synth_labels.json', 'synth_labels_generated.json'),
            'kick': ('kick_labels.json',)}
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

    @property
    def content_id(self):
        """What the song's audio is: its source bytes, section bounds and rate. Caches key on this, never on id."""
        key = dict(sha=list(self.source_sha256), start=self.source.get('start_s'), end=self.source.get('end_s'),
                   sr=self.sample_rate, how=self.source['how'])
        return hashlib.sha256(json.dumps(key, sort_keys=True).encode()).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


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
