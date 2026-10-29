#!/usr/bin/env python3
"""Whole-tree scan for the catalogue: every audio file and every Ableton set under the music folder, by stat only
(an online-only file is never opened; local files get the catalogue's hash, duration and loudness through
catalog.file_meta). Writes assets (shared with the registry songs), als_files (stat columns; als_scan fills the rest) and
skipped (folders deliberately not entered, with what they hold).

Skipped on purpose:
- top-level sample libraries and caches (Samples, Splice, User Library, Ableton): not Peter's compositions;
- inside a project: Samples, Backup, Ableton Project Info, Folder Info (recordings, freezes, auto-saves), pack folders
  (names with "sample" or "mini pack"), Presets.
Third-party stem packs under Old Albums Remix Stems And Extras/Remix Stems are scanned: they are remix material.

The music folder is read only: run under the sandbox-exec wrapper in catalog.py's docstring."""
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

SKIP_TOP = {
    'Samples': 'sample libraries (third-party packs, not compositions)',
    'Splice': 'Splice sample and preset cache',
    'User Library': 'Ableton user library (empty)',
    'Ableton': 'Ableton install data: factory packs, cloud manager, Live Recordings temp sessions (scratch takes)',
}
SKIP_DIR = re.compile(r'^(?!.*project$).*sample|^backup$|^ableton project info$|^folder info$|__macosx|mini pack|^presets$|'
                      r'^(imported|recorded|processed)$', re.I)
SKIP_WHY = (('imported', 'project imports (samples)'), ('recorded', 'project recordings (raw takes)'),
            ('processed', 'project processed samples'), ('sample', 'sample or pack folder'), ('mini pack', 'sample pack'), ('backup', 'Ableton auto-save backups'),
            ('project info', 'Ableton project info'), ('folder info', 'Ableton folder info'), ('preset', 'plugin presets'),
            ('__macosx', 'archive junk'))
AUDIO = {'.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a'}
REFERENCE = re.compile(r'original mix|acapella|accapella|\[vocals\]|\[music\]|\(vocals\)|_\(vocals\)|-vocals|diy acapella|mp3 320|'
                       r'\(instrumental\)|studio acapella|edmlake|^audio \[|the_weeknd|_-_', re.I)


def why(name):
    low = name.lower()
    for key, reason in SKIP_WHY:
        if key in low:
            return reason
    return 'skipped'


def count_audio(root):
    """(audio files, bytes) under root by name and stat only."""
    n = size = 0
    for d, _, fs in os.walk(root, onerror=lambda e: None):
        for f in fs:
            if os.path.splitext(f)[1].lower() in AUDIO:
                n += 1
                try:
                    size += os.stat(os.path.join(d, f)).st_size
                except OSError:
                    pass
    return n, size


def walk(music):
    """(audio [(path, rel parents)], sets [path], skipped [(path, reason, audio files, bytes)]) under the music folder."""
    audio, sets, skipped = [], [], []
    music = str(music)

    def go(d, parents):
        try:
            entries = sorted(os.scandir(d), key=lambda e: e.name)
        except OSError:
            return
        for e in entries:
            if e.name.startswith('.') or e.name.startswith('Icon'):
                continue
            if e.is_dir(follow_symlinks=False):
                if SKIP_DIR.search(e.name):
                    n, size = count_audio(e.path)
                    skipped.append((e.path, why(e.name), n, size))
                    continue
                go(e.path, parents + [e.name])
            elif e.name.endswith('.als'):
                sets.append(e.path)
            elif os.path.splitext(e.name)[1].lower() in AUDIO:
                audio.append((e.path, parents))
    for top in sorted(os.listdir(music)):
        p = os.path.join(music, top)
        if not os.path.isdir(p) or top.startswith('.'):
            continue
        if top in SKIP_TOP:
            n, size = count_audio(p)
            skipped.append((p, SKIP_TOP[top], n, size))
            continue
        go(p, [top])
    return audio, sets, skipped


def project_dir(path, music):
    """The nearest ancestor folder that is an Ableton project (its name ends " Project", or holds its own set), else None."""
    d = Path(path).parent
    music = Path(music)
    while d != music and music in d.parents:
        if d.name.endswith(' Project') or d.name in ('Oprression', 'Jordi Acoustic Edit', 'Blender Tests'):
            return str(d)
        d = d.parent
    return None


def scan(con, music=None, meta=True):
    """Scan the tree and upsert assets, als_files and skipped. Returns (audio files, sets, skipped dirs)."""
    from tools.audio_analysis.catalog import catalog
    music = Path(music or catalog.MUSIC)
    audio, sets, skipped = walk(music)
    prev = {r[0]: dict(zip(('size', 'mtime_ns', 'sha256', 'duration_s', 'sample_rate', 'channels', 'peak_dbfs', 'lufs'), r[1:]))
            for r in con.execute('SELECT path,size,mtime_ns,sha256,duration_s,sample_rate,channels,peak_dbfs,lufs FROM assets WHERE local=1')}
    todo = 0
    for i, (path, parents) in enumerate(audio):
        path = os.path.normpath(path)
        rel = parents[1:] if parents and parents[0] == 'Ableton Projects' else parents
        role, variant = catalog.classify(os.path.basename(path), rel, False)
        st = os.stat(path)
        dataless = catalog.dataless(st)
        if meta or dataless:
            m = catalog.file_meta(path, prev.get(path))
            if not dataless and not (prev.get(path) and prev[path].get('sha256')):
                todo += 1
                print(f'  [{i + 1}/{len(audio)}] measured {os.path.basename(path)}', flush=True)
        else:
            m = dict(size=st.st_size, mtime_ns=st.st_mtime_ns, local=0 if dataless else 1, sha256=None, duration_s=None,
                     sample_rate=None, channels=None, peak_dbfs=None, lufs=None)
        row = dict(path=path, name=os.path.basename(path), role=role, variant=variant,
                   version=catalog.parse_version(os.path.basename(path), rel), origin='music', **m)
        old = con.execute('SELECT role FROM assets WHERE path=?', (path,)).fetchone()
        if old:
            row.pop('role')            # a registry song's own role call stands
        with con:
            catalog.upsert(con, 'assets', ('path',), row)
    with con:
        for path in sets:
            st = os.stat(path)
            old = con.execute('SELECT mtime_ns FROM als_files WHERE path=?', (path,)).fetchone()
            row = dict(path=path, project_dir=project_dir(path, music) or str(Path(path).parent), name=os.path.basename(path),
                       size=st.st_size, mtime_ns=st.st_mtime_ns, local=0 if catalog.dataless(st) else 1)
            if old and old[0] != st.st_mtime_ns:
                row.update(scanned_mtime_ns=None, verdict=None, error=None)
            catalog.upsert(con, 'als_files', ('path',), row)
        con.execute('DELETE FROM als_files WHERE path NOT IN (%s)' % ','.join('?' * len(sets)), sets) if sets else None
        keep = [os.path.normpath(p) for p, _ in audio]
        con.execute('CREATE TEMP TABLE IF NOT EXISTS seen(path TEXT PRIMARY KEY)')
        con.execute('DELETE FROM seen')
        con.executemany('INSERT OR IGNORE INTO seen VALUES (?)', [(p,) for p in keep])
        con.execute("DELETE FROM assets WHERE origin='music' AND path LIKE ? AND path NOT IN (SELECT path FROM seen) "
                    "AND path NOT IN (SELECT path FROM song_assets)", (str(music) + '/%',))
        con.execute('DELETE FROM skipped')
        for p, reason, n, size in skipped:
            con.execute('INSERT OR REPLACE INTO skipped VALUES (?,?,?,?)', (p, reason, n, size))
    print(f'{len(audio)} audio files ({todo} newly measured), {len(sets)} sets, {len(skipped)} skipped folders')
    return len(audio), len(sets), len(skipped)


def scan_sets(con, only=(), force=False):
    """Run als_scan over every local Ableton set whose modified time moved since its last scan; store the summary.
    An online-only set is never opened (hydrate it with download first, when its composition needs it)."""
    import json
    import time

    from tools.audio_analysis.catalog import als_scan, catalog
    rows = con.execute('SELECT path, mtime_ns, scanned_mtime_ns FROM als_files WHERE local=1 ORDER BY size').fetchall()
    todo = [r for r in rows if (force or r[1] != r[2]) and (not only or any(o in r[0] for o in only))]
    print(f'{len(todo)} sets to scan ({len(rows)} local)', flush=True)
    for i, (path, mtime, _) in enumerate(todo):
        t0 = time.time()
        try:
            r = als_scan.scan(path)
            rd = r['ready']
            tracks = [dict(id=t['id'], kind=t['kind'], name=t['name'], groups=t['groups'], muted=t['muted'], part=t['part'],
                           midi=t.get('midi') and dict(notes=sum(t['midi']['by_voice'].values()), by_voice=t['midi']['by_voice'],
                                                       unknown=t['midi']['unknown'], first_s=t['midi'].get('first_s'),
                                                       last_s=t['midi'].get('last_s'), pads=t['midi']['pads'][:12]),
                           audio=t.get('audio') and dict(clips=t['audio']['clips'], files=t['audio']['files'][:3]))
                      for t in r['tracks'] if t['kind'] != 'GroupTrack' and (t.get('midi') or t.get('audio'))]
            row = dict(path=path, scanned_mtime_ns=mtime, creator=r['creator'], tempo=r['tempo'], tempo_min=r['tempo_min'],
                       tempo_max=r['tempo_max'], arrangement_s=r['arrangement_s'], verdict=rd['verdict'], kick_notes=rd['kick_notes'],
                       snare_notes=rd['snare_notes'], hat_notes=rd['hat_notes'], perc_notes=rd['perc_notes'],
                       muted_kick=rd['muted_kick'], muted_snare=rd['muted_snare'], audio_drums=catalog.j(rd['audio_drums']),
                       tracks=catalog.j(tracks), error=None)
        except Exception as e:      # a set from an old Live version or a broken file is recorded, not fatal
            row = dict(path=path, scanned_mtime_ns=mtime, verdict='error', error=f'{type(e).__name__}: {e}'[:200])
        with con:
            catalog.upsert(con, 'als_files', ('path',), row)
        print(f"  [{i + 1}/{len(todo)}] {row.get('verdict'):10s} {time.time() - t0:5.1f}s {os.path.basename(path)}", flush=True)


def download(con, paths, why):
    """Bring named online-only files down by reading them once (Dropbox fetches a file when it is read), then record each
    in `downloads` with its size and the reason, and refresh its catalogue row. Only audio and Ableton sets inside the music
    folder; a file that is already local is left alone. Never a folder."""
    import datetime

    from tools.audio_analysis.catalog import catalog
    music = str(catalog.MUSIC) + os.sep
    done = []
    for p in paths:
        p = os.path.normpath(p)
        ext = os.path.splitext(p)[1].lower()
        if not p.startswith(music) or os.path.isdir(p) or (ext not in AUDIO and ext != '.als'):
            raise SystemExit(f'refusing {p}: not an audio file or Ableton set inside the music folder')
        st = os.stat(p)
        if not catalog.dataless(st):
            print(f'  already local: {os.path.basename(p)}')
            continue
        with open(p, 'rb') as f:
            while f.read(1 << 24):
                pass
        st2 = os.stat(p)
        if catalog.dataless(st2):
            print(f'  still online-only after reading: {p}')
            continue
        at = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='seconds')
        with con:
            con.execute('INSERT OR REPLACE INTO downloads VALUES (?,?,?,?)', (p, st2.st_size, at, why))
            if ext == '.als':
                con.execute('UPDATE als_files SET local=1, size=?, mtime_ns=?, scanned_mtime_ns=NULL WHERE path=?',
                            (st2.st_size, st2.st_mtime_ns, p))
        if ext != '.als':
            m = catalog.file_meta(p)
            with con:
                con.execute('UPDATE assets SET local=1, size=?, mtime_ns=?, sha256=?, duration_s=?, sample_rate=?, channels=?, '
                            'peak_dbfs=?, lufs=? WHERE path=?', (m['size'], m['mtime_ns'], m['sha256'], m['duration_s'],
                                                                 m['sample_rate'], m['channels'], m['peak_dbfs'], m['lufs'], p))
        print(f'  downloaded {st2.st_size / 1e6:.1f} MB: {os.path.basename(p)}', flush=True)
        done.append(p)
    return done
