#!/usr/bin/env python3
"""Snare/clap one-shot sample files the projects use: drum-rack pads named snare/clap and one-shot snare/clap audio
clips. Read only: parses the .als XML in memory (gzip read), writes nothing near the projects. Missing absolute paths
are re-resolved from RelativePath and from the OneDrive -> Dropbox move. Prints OK/MISSING per file; with --json OUT
writes the resolved existing files."""
import gzip
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.kick_goal_melodic import _source  # noqa: E402

SNARE = re.compile(r'snare|clap|snr|clp', re.I)
BAD = re.compile(r'rim|snap|xstick|cross|roll|fill|loop|top|break|freeze|consolidate', re.I)
MOVES = (('/Users/peterkiemann/Library/CloudStorage/OneDrive-Personal/Documents/Music Production/',
          '/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/'),)


def resolve(ref, als_dir):
    cands = []
    p = ref.find('Path')
    if p is not None and p.get('Value'):
        cands.append(p.get('Value'))
        for a, b in MOVES:
            if cands[0].startswith(a):
                cands.append(b + cands[0][len(a):])
    r = ref.find('RelativePath')
    if r is not None and r.get('Value'):
        cands.append(str((als_dir / r.get('Value')).resolve()))
    for c in cands:
        if Path(c).exists():
            return c, True
    return (cands[0] if cands else None), False


def name_of(el):
    n = el.find('Name/EffectiveName')
    if n is None:
        n = el.find('Name')
    return (n.get('Value') if n is not None else None) or ''


def main():
    args = sys.argv[1:]
    out = args[args.index('--json') + 1] if '--json' in args else None
    songs = [a for a in args if a != '--json' and a != out]
    found = {}
    for t in songs:
        src = _source(t)
        if not src:
            continue
        als = Path(src[0])
        root = ET.fromstring(gzip.open(als).read())
        for br in root.iter():
            if not br.tag.endswith('Branch'):
                continue
            nm = name_of(br)
            if not SNARE.search(nm) or BAD.search(nm):
                continue
            for ref in br.iter('FileRef'):
                p, ok = resolve(ref, als.parent)
                if p and not BAD.search(Path(p).name):
                    found.setdefault((p, ok), set()).add(f'{t}: pad {nm}')
        for clip in root.iter('AudioClip'):
            for ref in clip.iter('FileRef'):
                p, ok = resolve(ref, als.parent)
                if p and SNARE.search(Path(p).name) and not BAD.search(Path(p).name):
                    found.setdefault((p, ok), set()).add(f'{t}: clip')
    for (p, ok), who in sorted(found.items()):
        print(('OK      ' if ok else 'MISSING ') + Path(p).name, '|', ', '.join(sorted(who))[:140])
    if out:
        # One entry per file name: an existing path and every song that uses the file (from any of its paths).
        by_name = {}
        for (p, ok), who in found.items():
            if Path(p).suffix.lower() not in (".wav", ".aif", ".aiff", ".flac"):
                continue
            e = by_name.setdefault(Path(p).name, dict(path=None, users=set()))
            e["users"] |= {w.split(":")[0] for w in who}
            if ok:
                e["path"] = p
        Path(out).write_text(json.dumps({n: dict(path=e["path"], users=sorted(e["users"])) for n, e in by_name.items() if e["path"]}, indent=1))


if __name__ == '__main__':
    main()
