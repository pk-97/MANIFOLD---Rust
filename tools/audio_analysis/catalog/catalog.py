#!/usr/bin/env python3
"""The detector data catalogue: every registry song, every audio file that belongs to it, what each file is, and how
songs relate. One SQLite file (catalog.sqlite, committed). `build` upserts the derived columns from a scan and never
touches the manual ones (songs.notes, songs.group_override, songs.licence_override, groups.note); the schema's
migrations live here. Nothing in this module opens an .als, and no online-only (dataless) file is ever opened.

The music folder is read only: run every command that scans it under
  sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath "/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production"))'

Usage:
  catalog.py build [SONG...]        scan and upsert (all songs when none named); then infer groups
  catalog.py show SONG              one song: files, lab mix, labels, licence, group, checks
  catalog.py find [--role R] [--local] [--online] [--kind project|generated] [--licence L] [--song S]
  catalog.py stats                  counts per role, licence and local state, hours, groups, remix-ready songs
  catalog.py sql "SELECT ..."       read-only ad-hoc query
  catalog.py dump                   stable text view of every table, for review diffs
Whole tree (every composition, lab or not; tree.py comps.py als_scan.py variants.py usable.py):
  catalog.py scan-tree [--no-meta]  every audio file and Ableton set under the music folder (stat only for online-only)
  catalog.py scan-sets [NAME...]    read the local Ableton sets (kick/snare/hat MIDI, audio drums, tempo), changed ones only
  catalog.py comps                  infer compositions from names; match each bounce to its probable set
  catalog.py variants [SONG...]     judge every local master/mix/WIP of a lab song against its lab mix (same_arrangement...)
  catalog.py stemsum [SONG...]      judge each stem folder's sum against the composition's bounces
  catalog.py download PATH... --why TEXT   bring named online-only files down (logged in `downloads`)
  catalog.py usable [COMP] [--kind K] [--lab|--ready|--blocked|--remix]   what training material each composition has
Library: stems(song_id) -> {role: [Path]} for the song's chosen stem set (local files only)."""
import argparse
import hashlib
import json
import math
import os
import re
import sqlite3
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

DB = Path(__file__).with_name('catalog.sqlite')
MUSIC = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production')
ABL = MUSIC / 'Ableton Projects'
STEMS_ROOT = ABL / 'STEMS'
REMIX_PACKS = MUSIC / 'Old Albums Remix Stems And Extras'
RELEASES = (MUSIC / 'EMERGENCE - ALBUM 2026', MUSIC / 'Interim - LS X EE - EP', MUSIC / 'Album Draft 2027')
ENV_CACHE = Path.home() / '.cache/manifold/detector-lab/catalog'
SF_DATALESS = 0x40000000
AUDIO_EXT = {'.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a'}
JUNK_DIRS = {'groups'}
JUNK_SUB = ('sample', 'backup', 'project info', 'folder info', '__macosx')   # packs and project internals
STEM_ROLES = ('drums', 'kick', 'snare', 'bass', 'other', 'nodrums', 'nokick')


# ---- schema ----------------------------------------------------------------------------------------------------

MIGRATIONS = [
    # 1
    """
    CREATE TABLE songs(
      id TEXT PRIMARY KEY, title TEXT, kind TEXT, content_id TEXT, sample_rate INTEGER, offset_s REAL, proof TEXT,
      audit_override TEXT, families TEXT, source TEXT, stem_set TEXT, lab_mix TEXT, label_inputs TEXT, generated TEXT,
      licence TEXT, licence_reason TEXT, licence_flags TEXT, group_id TEXT, group_basis TEXT,
      notes TEXT NOT NULL DEFAULT '', group_override TEXT, licence_override TEXT);
    CREATE TABLE label_counts(
      song_id TEXT, family TEXT, file TEXT, positives INTEGER, hard_neg INTEGER, unscored_s REAL,
      positives_only INTEGER, builder TEXT, PRIMARY KEY(song_id, family, file));
    CREATE TABLE assets(
      path TEXT PRIMARY KEY, name TEXT, role TEXT, variant TEXT, version TEXT, size INTEGER, mtime_ns INTEGER,
      local INTEGER, sha256 TEXT, duration_s REAL, sample_rate INTEGER, channels INTEGER, peak_dbfs REAL, lufs REAL,
      origin TEXT);
    CREATE TABLE song_assets(song_id TEXT, path TEXT, via TEXT, PRIMARY KEY(song_id, path));
    CREATE TABLE groups(id TEXT PRIMARY KEY, kind TEXT, size INTEGER, note TEXT NOT NULL DEFAULT '');
    CREATE TABLE group_edges(a TEXT, b TEXT, how TEXT, z REAL, PRIMARY KEY(a, b, how));
    CREATE TABLE checks(
      song_id TEXT, name TEXT, lag_s REAL, lag_samples INTEGER, corr REAL, residual_db REAL, raw_residual_db REAL,
      gain_db REAL, verdict TEXT, detail TEXT, PRIMARY KEY(song_id, name));
    CREATE TABLE renders(
      key TEXT PRIMARY KEY, song_id TEXT, seed INTEGER, recipe_version TEXT, path TEXT, sha256 TEXT, sources TEXT,
      params TEXT);
    CREATE VIEW song_groups AS SELECT id AS song_id, COALESCE(group_override, group_id) AS grp,
      COALESCE(licence_override, licence) AS lic FROM songs;
    """,
    # 2: the whole tree: compositions (lab or not), Ableton sets, where each bounce's set is, variants, downloads
    """
    CREATE TABLE als_files(
      path TEXT PRIMARY KEY, project_dir TEXT, name TEXT, size INTEGER, mtime_ns INTEGER, local INTEGER,
      scanned_mtime_ns INTEGER, creator TEXT, tempo REAL, tempo_min REAL, tempo_max REAL, arrangement_s REAL,
      verdict TEXT, kick_notes INTEGER, snare_notes INTEGER, hat_notes INTEGER, perc_notes INTEGER,
      muted_kick INTEGER, muted_snare INTEGER, audio_drums TEXT, tracks TEXT, error TEXT);
    CREATE TABLE comps(
      id TEXT PRIMARY KEY, title TEXT, kind TEXT, basis TEXT, aliases TEXT, groups TEXT, note TEXT NOT NULL DEFAULT '',
      kind_override TEXT);
    CREATE TABLE comp_assets(comp_id TEXT, path TEXT, via TEXT, flags TEXT, PRIMARY KEY(comp_id, path));
    CREATE TABLE comp_als(comp_id TEXT, path TEXT, PRIMARY KEY(comp_id, path));
    CREATE TABLE als_matches(audio_path TEXT PRIMARY KEY, als_path TEXT, how TEXT, gap_days REAL, len_diff_s REAL);
    CREATE TABLE variants(
      song_id TEXT, ref TEXT, path TEXT, lag_s REAL, lag_z REAL, corr REAL, residual_db REAL, raw_residual_db REAL,
      seg_frac REAL, drift_ms REAL, dur_s REAL, ref_dur_s REAL, verdict TEXT, detail TEXT, key TEXT,
      PRIMARY KEY(song_id, ref, path));
    CREATE TABLE downloads(path TEXT PRIMARY KEY, size INTEGER, at TEXT, why TEXT);
    CREATE TABLE skipped(path TEXT PRIMARY KEY, reason TEXT, audio_files INTEGER, bytes INTEGER);
    """,
    # running findings of the whole-tree work, so none of it lives only in a session
    """
    CREATE TABLE IF NOT EXISTS findings(topic TEXT NOT NULL, at TEXT NOT NULL, text TEXT NOT NULL, PRIMARY KEY(topic, at));
    """,
]
DERIVED_SONG = ('title', 'kind', 'content_id', 'sample_rate', 'offset_s', 'proof', 'audit_override', 'families',
                'source', 'stem_set', 'lab_mix', 'label_inputs', 'generated', 'licence', 'licence_reason',
                'licence_flags', 'group_id', 'group_basis')


def connect(path=None, readonly=False):
    path = Path(path or DB)
    if readonly:
        return sqlite3.connect(f'file:{path}?mode=ro', uri=True)
    con = sqlite3.connect(path)
    con.execute('CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL)')
    row = con.execute('SELECT MAX(version) FROM schema_version').fetchone()
    have = row[0] or 0
    for n in range(have, len(MIGRATIONS)):
        con.executescript(MIGRATIONS[n])
        con.execute('INSERT INTO schema_version VALUES (?)', (n + 1,))
    con.commit()
    return con


def j(x):
    return json.dumps(x, sort_keys=True, separators=(',', ':'))


def upsert(con, table, key_cols, row):
    """Insert the row, or update only the columns given (so manual columns that are not in `row` survive)."""
    cols = list(row)
    upd = ', '.join(f'{c}=excluded.{c}' for c in cols if c not in key_cols)
    sql = (f'INSERT INTO {table}({",".join(cols)}) VALUES({",".join("?" * len(cols))}) '
           f'ON CONFLICT({",".join(key_cols)}) DO ' + (f'UPDATE SET {upd}' if upd else 'NOTHING'))
    con.execute(sql, [row[c] for c in cols])


# ---- filenames: roles and versions -------------------------------------------------------------------------------

def norm(s):
    return re.sub(r'[^a-z0-9]', '', s.lower())


def _w(*words):
    return re.compile(r'(?<![a-z])(?:' + '|'.join(words) + r')(?![a-z])', re.I)


NODRUMS = re.compile(r'(?<![a-z])no[ _-]*drums?', re.I)
NOKICK = re.compile(r'(?<![a-z])no[ _-]*kicks?', re.I)
MIXMINUS = re.compile(r'(?<![a-z])no[ _-]+(?:vox|vocals?|leads?|pianos?|chords?|bass|synths?|guitars?|fx|arps?|melody)(?![a-z])'
                      r'|second verse', re.I)
KICK = re.compile(r'kick|(?<![a-z])kik(?![a-z])', re.I)
SNARE = re.compile(r'snare|clap|(?<![a-z])snr(?![a-z])', re.I)
DRUMS = re.compile(r'drum|perc|conga|snap|beatbox|fill|' + _w('hats?', 'hh', 'toms?', 'ride', 'crash', 'china', 'splash',
                   'overhead', 'room', 'shakers?', 'tamb', 'tambourine', 'cymbals?', 'bongos?', 'rim').pattern, re.I)
BASS = _w('bass(?:line)?', 'subs?', '808s?')
PREMASTER = re.compile(r'pre[ _-]?master', re.I)
MASTER = re.compile(r'(?<![a-z])(?:master|mastered|mstr)', re.I)
FIXTURE_ROLE = {'bass': 'bass', 'drums': 'drums', 'others': 'other', 'vocals': 'other', 'mix': 'full_mix'}


def classify(name, parents=(), stem_dir=False):
    """(role, variant) from a file name and the folder names under the scan root. Roles: drums kick snare bass other
    nodrums nokick full_mix master premaster. variant names a mix-minus bounce ("no vox")."""
    stem = Path(name).stem
    low = stem.lower()
    if NODRUMS.search(stem):
        return 'nodrums', None
    if NOKICK.search(stem):
        return 'nokick', None
    m = MIXMINUS.search(stem)
    if m:
        return 'full_mix', m.group(0).lower().strip()
    if low in FIXTURE_ROLE and any(p.startswith('fixture:') for p in parents):
        return FIXTURE_ROLE[low], None
    for role, rx in (('kick', KICK), ('snare', SNARE), ('drums', DRUMS), ('bass', BASS)):
        if rx.search(stem):
            return role, None
    if MASTER.search(stem):
        return 'master', None
    if PREMASTER.search(stem):
        return 'premaster', None
    if stem_dir or 'stem' in low or any('stem' in p.lower() for p in parents):
        return 'other', None
    return 'full_mix', None


def parse_version(name, parents=()):
    """Version tag from the file name, else its folders: 'WIP 4', 'PREMASTER T2', 'MASTER T1', 'FINAL V3', 'MSTR V3'..."""
    def tags(s):
        out = []
        for rx, fmt in ((r'wip\s*(\d+)', lambda m: f'WIP {m.group(1)}'),
                        (r'pre[ _-]?master', lambda m: 'PREMASTER'),
                        (r'(?<![a-z0-9])t([1-9])(?![0-9a-z])', lambda m: f'T{m.group(1)}'),
                        (r'mstr', lambda m: 'MSTR'),
                        (r'(?<![a-z])master(?:ed)?(?![a-z])', lambda m: 'MASTER'),
                        (r'(?<![a-z])final(?![a-z])', lambda m: 'FINAL'),
                        (r'(?<![a-z0-9])v([0-9]+)(?![0-9a-z])', lambda m: f'V{m.group(1)}')):
            for m in re.finditer(rx, s, re.I):
                t = fmt(m)
                if t not in out:
                    out.append(t)
        return out
    t = tags(Path(name).stem)
    if not t:
        for p in reversed(list(parents)[-2:]):
            t = tags(p)
            if t:
                break
    return ' '.join(t) or None


# ---- files: metadata, never opening online-only files ---------------------------------------------------------------

def is_dataless(path):
    return bool(os.stat(path).st_flags & SF_DATALESS)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def k_weight_sos(fs):
    """BS.1770 K-weighting as two second-order sections at any sample rate."""
    import numpy as np
    f0, G, Q = 1681.974450955533, 3.999843853973347, 0.7071752369554196
    K = math.tan(math.pi * f0 / fs)
    Vh = 10 ** (G / 20)
    Vb = Vh ** 0.4996667741545416
    a0 = 1 + K / Q + K * K
    shelf = [(Vh + Vb * K / Q + K * K) / a0, 2 * (K * K - Vh) / a0, (Vh - Vb * K / Q + K * K) / a0,
             1.0, 2 * (K * K - 1) / a0, (1 - K / Q + K * K) / a0]
    f0, Q = 38.13547087602444, 0.5003270373238773
    K = math.tan(math.pi * f0 / fs)
    d = 1 + K / Q + K * K
    hp = [1.0, -2.0, 1.0, 1.0, 2 * (K * K - 1) / d, (1 - K / Q + K * K) / d]
    return np.array([shelf, hp])


def loudness_peak(path):
    """(integrated LUFS or None, sample peak dBFS or None) in one streaming pass (BS.1770-4, gated)."""
    import numpy as np
    import soundfile as sf
    from scipy.signal import sosfilt
    with sf.SoundFile(path) as f:
        sr, ch = f.samplerate, min(f.channels, 2)
        sos = k_weight_sos(sr)
        zi = [np.zeros((sos.shape[0], 2)) for _ in range(ch)]
        hop = sr // 10
        peak, rem, ms = 0.0, np.zeros((0, ch)), []
        for blk in f.blocks(blocksize=hop * 100, dtype='float64', always_2d=True):
            blk = blk[:, :ch]
            peak = max(peak, float(np.abs(blk).max(initial=0.0)))
            y = np.empty_like(blk)
            for c in range(ch):
                y[:, c], zi[c] = sosfilt(sos, blk[:, c], zi=zi[c])
            y = np.concatenate([rem, y])
            nb = len(y) // hop
            if nb:
                ms.append((y[:nb * hop] ** 2).reshape(nb, hop, ch).mean(axis=1))
            rem = y[nb * hop:]
    pk = 20 * math.log10(peak) if peak > 0 else None
    if not ms:
        return None, pk
    ms = np.concatenate(ms)
    if len(ms) < 4:
        return None, pk
    z = np.stack([ms[i:len(ms) - 3 + i] for i in range(4)]).mean(axis=0).sum(axis=1)
    with np.errstate(divide='ignore'):
        lk = -0.691 + 10 * np.log10(z)
    keep = lk > -70
    if not keep.any():
        return None, pk
    gate = -0.691 + 10 * math.log10(z[keep].mean()) - 10
    keep &= lk > gate
    if not keep.any():
        return None, pk
    return float(-0.691 + 10 * math.log10(z[keep].mean())), pk


def file_meta(path, memo=None):
    """Size, mtime, local flag and, for local files, sha256, duration, rate, channels, peak and loudness. An online-only
    file is never opened: stat only."""
    st = os.stat(path)
    meta = dict(size=st.st_size, mtime_ns=st.st_mtime_ns, local=0, sha256=None, duration_s=None, sample_rate=None,
                channels=None, peak_dbfs=None, lufs=None)
    if is_dataless(path):
        return meta
    meta['local'] = 1
    if memo and memo.get('size') == st.st_size and memo.get('mtime_ns') == st.st_mtime_ns and memo.get('sha256'):
        return {**meta, **{k: memo[k] for k in ('sha256', 'duration_s', 'sample_rate', 'channels', 'peak_dbfs', 'lufs')}}
    meta['sha256'] = sha256_file(path)
    try:
        import soundfile as sf
        info = sf.info(path)
        meta.update(duration_s=round(info.duration, 3), sample_rate=info.samplerate, channels=info.channels)
        lufs, pk = loudness_peak(path)
        meta.update(lufs=None if lufs is None else round(lufs, 2), peak_dbfs=None if pk is None else round(pk, 2))
    except Exception as e:  # unreadable audio stays in the catalogue with its hash and no audio facts
        print(f'  unreadable audio {path}: {e}', file=sys.stderr)
    return meta


# ---- scanning -----------------------------------------------------------------------------------------------------

def scan_audio(root, depth, stem_root=False):
    """Audio files under root down to `depth` folder levels, as (path, parents, stem_dir). Junk folders (project
    samples, backups) and online-only folders are not entered."""
    out = []

    def walk(d, level, parents):
        try:
            it = sorted(os.scandir(d), key=lambda e: e.name)
        except OSError:
            return
        for e in it:
            if e.name.startswith('.'):
                continue
            if e.is_dir(follow_symlinks=False):
                if (level < depth and e.name.lower() not in JUNK_DIRS and not any(x in e.name.lower() for x in JUNK_SUB)
                        and not (e.stat().st_flags & SF_DATALESS)):
                    walk(e.path, level + 1, parents + [e.name])
            elif Path(e.name).suffix.lower() in AUDIO_EXT:
                out.append((e.path, parents, stem_root))
    if os.path.isdir(root):
        walk(str(root), 0, [])
    return out


def als_files(dirs):
    """Project .als files (path, mtime, size) by stat only."""
    out = []
    for d in dirs:
        try:
            names = sorted(os.listdir(d))
        except OSError:
            continue
        for n in names:
            if n.endswith('.als'):
                st = os.stat(os.path.join(d, n))
                out.append(dict(path=os.path.join(d, n), mtime_ns=st.st_mtime_ns, size=st.st_size))
    return out


# ---- the song list's context: titles, stem sets, label code facts ------------------------------------------------

TITLES = {
    'flight': ('Flight', []), '48_hours': ('48 Hours', []), 'pattern': ('Pattern', []),
    'back_to_you': ('Back to You', ['States']), 'lowkey': ('Lowkey', []), 'corrosion': ('Corrosion', ['Katsu Don']),
    'burn_stems': ('Burn', ['Fifty']), 'midnight_patience': ('Midnight Patience', ['Bettys']),
    'heavy_on_mind': ('Heavy on Mind', ['Texture']), 'miracle': ('Miracle', ['Streets']),
    'right_where': ('Right Where I Need You', ['Steady']), 'hung_up': ('Hung Up', ['Rainfall']),
    'got_it': ('Got It', ['MudPie']), 'renew': ('Renew', []), 'overwhelming_force': ('Overwhelming Force', []),
    'murmur': ('Murmur', ['snackpack']), 'default_haze': ('Default Haze', []), 'late_night': ('Late Night', []),
    'crush': ('Crush', []), 'dusk': ('Dusk', []), 'imitation': ('Imitation', ['Pusher']),
    'set_me_free': ('Set Me Free', ['Limerant']), 'openings': ('Openings', []), 'staged': ('Staged', []),
    'just_the_way': ('Just the Way', ['Stay The Night']), 'water_shimmer': ('Water Shimmer', []),
    'basalt': ('Basalt', []), 'cadence': ('Cadence', []), 'push': ('Push', ['Dimension']), 'stagnate': ('Stagnate', []),
    'weight': ('Weight', []), 'facade': ('Facade', []),
    'cold_remix': ('Cold (Eleonora remix)', []), 'worship': ('Worship', []), 'business': ('Business', []),
    'eat_sleep': ('Eat Sleep', []), 'lh': ('LH', []), 'gerrit': ('Gerrit', []),
}
THIRD_PARTY = ('cold_remix', 'worship', 'business', 'eat_sleep', 'lh', 'gerrit')
FIXTURES = ('apricots_128bpm', 'bad_guy_128bpm', 'feel_the_vibration_174bpm', 'inhale_exhale_145bpm', 'tears_140bpm')
RP = REMIX_PACKS / 'Remix Stems'
# song -> (folder, file-name regex or None): the files its remix renderer sums. Chosen so the sum is the lab mix
# (docs in remix.py); a song missing here has no usable stem set.
STEM_SETS = {
    'late_night': (STEMS_ROOT / 'LATE NIGHT STEMS', None),
    'midnight_patience': (STEMS_ROOT / 'MIDNIGHT PATIENCE STEMS', None),
    'heavy_on_mind': (STEMS_ROOT / 'HEAVY ON MIND STEMS', None),
    'miracle': (STEMS_ROOT / 'MIRACLE STEMS', None),
    'weight': (STEMS_ROOT / 'WEIGHT STEMS', None),
    'staged': (STEMS_ROOT / 'STAGED STEMS', None),
    'back_to_you': (ABL / '2024/States Project/STEMS/PRE MASTER', None),
    'burn_stems': (ABL / '2025/Fifty Project/STEMS 3-08-25', None),
    'pattern': (ABL / '2025/Pattern Project/MASTERS/32Bit', r'KICK STEM|NO KICK - 32Bit'),
    'corrosion': (ABL / '2026/Katsu Don Project/FINALS/PREMASTERS T2', None),
    'murmur': (MUSIC / 'Interim - LS X EE - EP/PREMASTERS T2', r'^04 - '),
    'right_where': (MUSIC / 'Interim - LS X EE - EP/PREMASTERS T2', r'^01 - '),
    'hung_up': (MUSIC / 'Interim - LS X EE - EP/PREMASTERS T2', r'^03 - '),
    'basalt': (ABL / '2025/Basalt Project/STEMS', r'WIP 4'),
    'cold_remix': (ABL / '2024/Eleonora - Cold - Remix Project/STEMS - Cold', None),
    'worship': (RP / 'Worship - MAX', None),
    'business': (RP / 'Tiësto - Business (STEMS)-selected', None),
    'eat_sleep': (RP / 'Kx5 - Eat Sleep', None),
    'lh': (RP / 'LH Stems', None),
    'gerrit': (ABL / 'Ableton 11/Gerrit Mixdown Project/STEMS', None),
}
COLLAB = re.compile(r'(?<![a-z])x(?![a-z])|&|feat|(?<![a-z])ft\.|binding|esplanade|(?<![a-z])ee(?![a-z])|eleonora|tayla',
                    re.I)
VOCAL = re.compile(r'vox|vocal|acapella', re.I)


def read_json(p):
    return json.loads(Path(p).read_text())


class Context:
    """Facts the label code and registry already hold, read once."""

    def __init__(self, songs):
        from tools.audio_analysis.eval.kick_goal_melodic import PROJECT, SURVEY, TRIGGER
        from tools.audio_analysis.eval.snare_labels import FAMILY
        from tools.audio_analysis.lab import registry
        self.registry = registry
        G = Path.home() / '.cache/manifold/kick-goal-2026-10-10'
        self.labels = {f: read_json(G / f) for fs in registry.FAMILIES.values() for f in fs if (G / f).exists()}
        self.news = {}
        for f in ('labels_v2', 'labels_more', 'labels_trigger', 'labels_wip', 'labels_wip2', 'labels_recall'):
            if (G / f'{f}.json').exists():
                for k, v in read_json(G / f'{f}.json')['new'].items():
                    self.news.setdefault(k, []).append((f, v))
        self.wipjson = read_json(G / 'snare_wip.json') if (G / 'snare_wip.json').exists() else {}
        self.project, self.trigger, self.survey = PROJECT, TRIGGER, SURVEY
        self.family = FAMILY
        self.drum_stems = {t: ([p] if not isinstance(p, (list, tuple)) else list(p)) for t, p in FAMILY.drum_stems}

    def als(self, t):
        out = []

        def add(a):
            a = a['path'] if isinstance(a, dict) else str(a)
            if a not in out:
                out.append(a)
        if t in self.project:
            add(self.project[t]['als'])
        if t in self.trigger:
            add(self.trigger[t]['als'])
        sp = self.survey / f'{t}.json'
        if sp.exists():
            add(read_json(sp)['als'])
        if t in self.wipjson:
            add(self.wipjson[t]['als'])
        for _, v in self.news.get(t, []):
            if v.get('als'):
                add(v['als'])
        return out

    def label_stem_files(self, t):
        """{path: use} for the stems the label code names for this song."""
        use = {}
        for p in self.drum_stems.get(t, []):
            use[str(p)] = 'snare drum stem'
        for _, v in self.news.get(t, []):
            if v.get('kick_stem'):
                for p in ([v['kick_stem']] if isinstance(v['kick_stem'], str) else v['kick_stem']):
                    use[str(p)] = 'kick stem'
            if str(v.get('mix_kind', '')).startswith('stem_sum'):
                for p in v.get('mix_parts') or []:
                    use.setdefault(str(p), 'lab mix part')
        return use


def match_title(t, name, aliases):
    n = norm(name)
    return any(len(norm(a)) >= 4 and norm(a) in n for a in aliases)


# ---- building a song's entry --------------------------------------------------------------------------------------

def song_entry(s, ctx, assets, prev_assets):
    """One song's derived row, its label counts, the (path, via) list of its files, adding file rows to `assets`."""
    t = s.id
    title, aliases = TITLES.get(t, (t.replace('_', ' ').title(), []))
    own = s.kind == 'project' and t not in THIRD_PARTY and t not in FIXTURES
    keys = [title] + aliases
    row = dict(id=t, title=title, kind=s.kind, content_id=s.content_id, sample_rate=s.sample_rate,
               offset_s=s.offset_s, proof=s.proof, audit_override=s.audit_override, families=j(list(s.families)))
    counts = []
    for fam in s.families:
        for f in ctx.registry.FAMILIES[fam]:
            r = ctx.labels.get(f, {}).get(t)
            if r is not None:
                counts.append(dict(song_id=t, family=fam, file=f, positives=len(r.get('positives', [])),
                                   hard_neg=len(r.get('hard_neg', [])),
                                   unscored_s=round(sum(b - a for a, b in r.get('unscored', [])), 2),
                                   positives_only=int(bool(r.get('positives_only'))), builder=r.get('builder')))
    files = []          # (path, parents, stem_dir, via)

    def gather(found, via):
        files.extend((p, par, sd, via) for p, par, sd in found)
    als = ctx.als(t)
    proj_dirs = []
    for a in als:
        d = str(Path(a).parent)
        if d not in proj_dirs and d.startswith(str(ABL)):
            proj_dirs.append(d)
    if s.kind == 'project' and t not in FIXTURES:
        if t in STEM_SETS:
            folder, rx = STEM_SETS[t]
            gather([f for f in scan_audio(folder, 0, stem_root=True) if not rx or re.search(rx, os.path.basename(f[0]))],
                   'stem set folder')
        label_dirs = {str(Path(p).parent) for p in ctx.label_stem_files(t) if os.path.exists(p)}
        for d in sorted(label_dirs):
            shared = d.startswith(str(MUSIC / 'Interim'))      # a release folder holds several songs
            gather([f for f in scan_audio(d, 0, stem_root=True) if not shared or match_title(t, os.path.basename(f[0]), keys)],
                   'label stem folder')
        for d in proj_dirs:
            gather(scan_audio(d, 3), 'project')
        if own and STEMS_ROOT.is_dir():
            for d in sorted(os.listdir(STEMS_ROOT)):
                if (STEMS_ROOT / d).is_dir() and match_title(t, d, keys):
                    gather(scan_audio(STEMS_ROOT / d, 2, stem_root=True), 'stems root')
        if own:
            for rel in RELEASES:
                gather([f for f in scan_audio(rel, 3) if match_title(t, Path(f[0]).name, keys)], 'release')
    mix = lab_mix_paths(s, ctx)
    for p in mix:
        if os.path.exists(p):
            gather([(p, [], False)], 'lab mix')
    seen, listed = set(), []
    for path, parents, sd, via in files:
        path = os.path.normpath(path)
        if path in seen:
            continue
        seen.add(path)
        if path not in assets:
            role, variant = classify(os.path.basename(path), parents, sd)
            origin = 'music' if path.startswith(str(MUSIC)) else 'repo' if path.startswith(str(ROOT)) or 'tests/fixtures' in path else 'cache'
            meta = file_meta(path, prev_assets.get(path))
            assets[path] = dict(path=path, name=os.path.basename(path), role=role, variant=variant,
                                version=parse_version(os.path.basename(path), parents), origin=origin, **meta)
        if via == 'project' and misfiled(t, keys, os.path.basename(path)):
            via = 'project (other title)'
        listed.append((path, via))
    if s.kind == 'project' and t in FIXTURES:
        d = Path(s.source['paths'][0]).parent
        for f in sorted(d.glob('*.wav')):
            path = str(f)
            role, variant = classify(f.name, ['fixture:audio'], True)
            assets[path] = dict(path=path, name=f.name, role=role, variant=variant, version=None, origin='repo',
                                **file_meta(path, prev_assets.get(path)))
            listed.append((path, 'fixture folder'))
    row['source'] = j(dict(project_dirs=proj_dirs, als=als_files(proj_dirs), als_recorded=als))
    row['stem_set'] = j(stem_set(t, listed, assets))
    row['lab_mix'] = j(lab_mix(s, ctx, assets))
    row['label_inputs'] = j(ctx.label_stem_files(t))
    row['generated'] = j(generated_info(s, ctx)) if s.kind == 'generated' else None
    row.update(licence_of(s, row, listed, assets))
    return row, counts, listed


def misfiled(t, own_keys, name):
    """A file in a project folder that carries another registry song's title (a copied bounce, a reused template)."""
    n = norm(name)
    if any(len(norm(k)) >= 4 and norm(k) in n for k in own_keys):
        return False
    return any(len(norm(k)) >= 5 and norm(k) in n for u, (ti, al) in TITLES.items() if u != t for k in [ti] + al)


def lab_mix_paths(s, ctx):
    t = s.id
    paths = []
    if s.source['how'] != 'section' and not s.source['paths'][0].endswith('.npy'):
        paths.append(s.source['paths'][0])
    for _, v in ctx.news.get(t, []):
        if v.get('mix_path'):
            paths.append(str(v['mix_path']))
        if v.get('mix_kind', '').startswith('wip') and v.get('mix_parts'):
            paths.append(str(v['mix_parts'][0]))
    if s.kind == 'generated':
        paths.append(s.source['paths'][0])
    if t in ctx.wipjson and ctx.wipjson[t].get('wip'):
        paths.append(str(ctx.wipjson[t]['wip']))
    return list(dict.fromkeys(paths))


def lab_mix(s, ctx, assets):
    """What the lab's mix for this song is, and whether a file on disk matches it."""
    t, src = s.id, s.source
    if s.kind == 'generated':
        p = src['paths'][0]
        a = assets.get(os.path.normpath(p))
        return dict(kind='section', file=p, start_s=src['start_s'], end_s=src['end_s'],
                    match='sha' if a and a['sha256'] == s.source_sha256[0] else 'none')
    p = src['paths'][0]
    if not p.endswith('.npy'):
        a = assets.get(os.path.normpath(p))
        return dict(kind='file', file=p, match='sha' if a and a['sha256'] == s.source_sha256[0] else 'none')
    for _, v in ctx.news.get(t, []):
        mk = v.get('mix_kind')
        if not mk:
            continue
        if mk.startswith('stem_sum'):
            parts = [str(q) for q in (v.get('mix_parts') or [])]
            ok = parts and all(os.path.exists(q) for q in parts)
            return dict(kind='stem_sum', parts=parts, cached=p, match='stem_sum' if ok else 'none')
        if mk == 'master':
            a = assets.get(os.path.normpath(str(v['mix_path'])))
            return dict(kind='file', file=str(v['mix_path']), match='sha' if a and a['sha256'] == v.get('mix_sha256') else 'none')
        if mk.startswith('wip'):
            f = (v.get('mix_parts') or [None])[0]
            a = assets.get(os.path.normpath(str(f))) if f else None
            ok = bool(a and a['local'])
            return dict(kind='wip bounce', file=f, cached=p, sha256=v.get('mix_sha256'),
                        match='sha' if ok and a['sha256'] == v.get('mix_sha256') else 'file exists' if ok else 'none')
    w = ctx.wipjson.get(t)
    if w:
        a = assets.get(os.path.normpath(str(w.get('wip'))))
        return dict(kind='wip bounce', file=w.get('wip'), cached=p, match='file exists' if a and a['local'] else 'none')
    return dict(kind='unknown', cached=p, match='none')


def stem_set(t, listed, assets):
    if t not in STEM_SETS:
        return None
    folder, rx = STEM_SETS[t]
    folder = os.path.normpath(str(folder))
    paths, online = [], []
    for path, _ in listed:
        a = assets[path]
        if os.path.dirname(path) != folder or a['role'] not in STEM_ROLES:
            continue
        if rx and not re.search(rx, a['name']):
            continue
        (paths if a['local'] else online).append(path)
    return dict(folder=folder, paths=sorted(set(paths)), online_only=sorted(set(online)))


def generated_info(s, ctx):
    exp = Path(s.source['paths'][0])
    proj = next((p for p in exp.parents if p.name.endswith(' Project')), exp.parent)
    return dict(export=str(exp), section=[s.source['start_s'], s.source['end_s']], project_dir=str(proj),
                als=als_files([proj]))


LICENCES = ('own', 'own_remix_private', 'own_collab', 'own_mix_job', 'commercial_excerpt', 'generated', 'cc0', 'cc_by', 'unknown')
# Peter's calls, seeded into the manual columns where they are still empty; a later edit by Peter is never overwritten.
SEED_MANUAL = {t: dict(licence_override='own_remix_private', notes='Peter 2026-10-12: private remix stems, kept in training')
               for t in ('cold_remix', 'worship', 'business', 'eat_sleep', 'lh')}
for _t in ('corrosion', 'right_where', 'hung_up', 'murmur', 'midnight_patience', 'heavy_on_mind', 'late_night', 'miracle'):
    SEED_MANUAL[_t] = dict(licence_override='own_collab', notes='Peter 2026-10-12: collaborations with Peter, fine to train')
SEED_MANUAL['gerrit'] = dict(licence_override='own_mix_job', notes='Peter 2026-10-12: fine to train')
for _t in ('apricots_128bpm', 'bad_guy_128bpm', 'feel_the_vibration_174bpm', 'inhale_exhale_145bpm', 'tears_140bpm'):
    SEED_MANUAL[_t] = dict(licence_override='commercial_excerpt', notes='Peter 2026-10-12: train on all for now')


def seed_manual(con, seeds=None):
    for t, cols in (seeds or SEED_MANUAL).items():
        for col, val in cols.items():
            con.execute(f"UPDATE songs SET {col}=? WHERE id=? AND ({col} IS NULL OR {col}='')", (val, t))


def licence_of(s, row, listed, assets):
    """(licence, reason, flags) from the files. Own work is 'own'; a named collaborator, other people's stems or an
    unrecorded source is 'unknown'. Nothing is ever guessed as permissive; Peter's calls live in licence_override."""
    t = s.id
    names = [row['title']] + [assets[p]['name'] for p, via in listed if via != 'project (other title)'] + [Path(a).name for a in json.loads(row['source'])['als_recorded']]
    flags = []
    if s.kind == 'generated':
        return dict(licence='generated', licence_reason='rendered from the Labels and Timings project (own); instrument and '
                    'sample licences inside it are not audited', licence_flags=j(['sample licences unaudited']))
    if t in FIXTURES:
        return dict(licence='unknown', licence_reason='short excerpt of a commercial recording with separated stems; '
                    'source and licence not recorded', licence_flags=j([]))
    if t in THIRD_PARTY:
        return dict(licence='unknown', licence_reason=f'stems supplied by other artists for a remix/mix '
                    f'({STEM_SETS[t][0].name}); the stems\' licence is not recorded', licence_flags=j(['third-party stems']))
    if any(VOCAL.search(assets[p]['name']) for p, _ in listed if assets[p]['role'] == 'other'):
        flags.append('vocal stems present: performer rights unconfirmed')
    hit = next((n for n in names if COLLAB.search(n)), None)
    if hit:
        return dict(licence='unknown', licence_reason=f'a collaborator may be named in "{hit}"; rights not recorded',
                    licence_flags=j(flags))
    return dict(licence='own', licence_reason='Peter\'s own project (Ableton Projects)', licence_flags=j(flags))


# ---- groups -------------------------------------------------------------------------------------------------------

BANDS = ((30, 120), (120, 400), (400, 1500), (1500, 5000), (5000, 12000))
AUDIO_NCC = 0.65        # same composition: real versions score 0.77-1.0, the best unrelated pair 0.57 (703 pairs)
FEAT_VERSION = 'feat100-1'


def feat100(x, sr):
    """Five band log-energies at 100 Hz, with the 3 s running mean removed and each band scaled to unit variance."""
    import numpy as np
    from scipy.ndimage import uniform_filter1d
    from scipy.signal import butter, sosfilt
    hop = sr // 100
    out = []
    for lo, hi in BANDS:
        hi = min(hi, sr / 2 * .95)
        y = sosfilt(butter(2, (lo, hi), btype='band', fs=sr, output='sos'), x) ** 2
        n = len(y) // hop
        e = 10 * np.log10(y[:n * hop].reshape(n, hop).mean(axis=1) + 1e-10)
        e = e - uniform_filter1d(e, 300)
        out.append(e / (e.std() + 1e-9))
    return np.stack(out, axis=1)


def pair_ncc(a, b, win_s=40):
    """Best normalised cross-correlation between an excerpt of a (three tried) and any stretch of b."""
    import numpy as np
    w = win_s * 100
    if len(a) < w or len(b) < w:
        return 0.0
    n = 1 << int(np.ceil(np.log2(len(b) + w)))
    fb = [np.fft.rfft(b[:, i], n) for i in range(b.shape[1])]
    e2 = np.cumsum(np.concatenate([[0.0], (b ** 2).sum(axis=1)]))
    win_e = e2[w:] - e2[:-w]
    best = 0.0
    for frac in (.25, .5, .75):
        ex = a[int(frac * (len(a) - w)):][:w]
        c = sum(np.fft.irfft(fb[i] * np.conj(np.fft.rfft(ex[:, i], n)), n)[:len(b) - w + 1] for i in range(b.shape[1]))
        ncc = c / np.sqrt((ex ** 2).sum() * np.maximum(win_e, 1e-9))
        ncc[win_e < .5 * w * b.shape[1]] = 0.0          # a stretch of near-silence has no shape to match
        best = max(best, float(np.clip(ncc, -1, 1).max()))
    return best


def song_feats(songs, ids):
    import numpy as np
    ENV_CACHE.mkdir(parents=True, exist_ok=True)
    out = {}
    for t in ids:
        s = songs[t]
        p = ENV_CACHE / f'{FEAT_VERSION}_{s.content_id[:24]}.npy'
        if not p.exists():
            from tools.audio_analysis.lab import registry
            np.save(p, feat100(registry.mix(s), s.sample_rate).astype(np.float32))
        out[t] = np.load(p).astype(np.float64)
    return out


def infer_groups(songs, rows, listed, assets, only_audio_pairs=None):
    """Union songs that are versions of one composition. Edges: name (shared title/alias/project folder), sha (a shared
    non-silent audio file), audio (an excerpt of one found in the other). Generated sections group by export file.
    Returns (group of each song, basis of each song, edges)."""
    ids = [t for t in rows if songs[t].kind == 'project' and t not in FIXTURES]
    keys = {}
    for t in ids:
        title, aliases = TITLES.get(t, (t, []))
        k = {norm(title)} | {norm(a) for a in aliases}
        for d in json.loads(rows[t]['source'])['project_dirs']:
            k.add(norm(re.sub(r' Project$', '', Path(d).name)))
        keys[t] = {x for x in k if len(x) >= 3}
    edges = {}

    def edge(a, b, how, z=None):
        a, b = sorted((a, b))
        edges[(a, b, how)] = z
    for i, a in enumerate(ids):
        for b in ids[i + 1:]:
            if keys[a] & keys[b]:
                edge(a, b, 'name')
    by_sha = {}
    for t in ids:
        for p, via in listed[t]:
            a = assets[p]
            if via != 'project (other title)' and a['sha256'] and a['lufs'] is not None and a['lufs'] > -60 and (a['duration_s'] or 0) > 20:
                by_sha.setdefault(a['sha256'], set()).add(t)
    for ts in by_sha.values():
        ts = sorted(ts)
        for i, a in enumerate(ts):
            for b in ts[i + 1:]:
                edge(a, b, 'sha')
    envs = song_feats(songs, ids)
    for i, a in enumerate(ids):
        for b in ids[i + 1:]:
            z = max(pair_ncc(envs[a], envs[b]), pair_ncc(envs[b], envs[a]))
            if z >= AUDIO_NCC * .6:
                edge(a, b, 'audio', round(z, 3))
    parent = {t: t for t in rows}

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x
    for (a, b, how), z in edges.items():
        if how != 'audio' or z >= AUDIO_NCC:
            parent[find(a)] = find(b)
    for t, r in rows.items():
        if songs[t].kind == 'generated':
            e = Path(songs[t].source['paths'][0]).stem
            parent[t] = 'gen_' + re.sub(r'[^a-z0-9]+', '_', e.lower()).strip('_')
        elif t in FIXTURES:
            parent[t] = t
    groups = {}
    for t in rows:
        g = find(t) if parent[t] in rows else parent[t]
        groups.setdefault(g, []).append(t)
    gid, basis = {}, {}
    for g, members in groups.items():
        name = g if g.startswith('gen_') else min(members)
        for t in members:
            gid[t] = name
            if g.startswith('gen_'):
                basis[t] = ['export']
            elif len(members) == 1:
                basis[t] = []
            else:
                hows = sorted({how for (a, b, how), z in edges.items() if (a == t or b == t) and (how != 'audio' or z >= AUDIO_NCC)})
                basis[t] = hows
    return gid, basis, edges


# ---- build --------------------------------------------------------------------------------------------------------

def build(only=(), con=None):
    from tools.audio_analysis.lab import registry
    con = con or connect()
    songs = registry.load()
    ctx = Context(songs)
    prev_assets = {r[0]: dict(zip(('size', 'mtime_ns', 'sha256', 'duration_s', 'sample_rate', 'channels', 'peak_dbfs', 'lufs'), r[1:]))
                   for r in con.execute('SELECT path,size,mtime_ns,sha256,duration_s,sample_rate,channels,peak_dbfs,lufs '
                                        'FROM assets WHERE local=1')}
    assets, rows, listed, counts = {}, {}, {}, {}
    todo = [t for t in songs if not only or t in only]
    for t in todo:
        print(f'scan {t}', flush=True)
        rows[t], counts[t], listed[t] = song_entry(songs[t], ctx, assets, prev_assets)
    for r in con.execute('SELECT song_id, path FROM song_assets'):
        if r[0] not in rows:
            listed.setdefault(r[0], []).append((r[1], 'kept'))
    # groups need every song's row; unscanned songs contribute their stored rows
    for sid, src, fam in con.execute('SELECT id, source, content_id FROM songs'):
        if sid not in rows and sid in songs:
            rows[sid] = dict(source=src)
            listed.setdefault(sid, [])
    stored = {}
    for p, role, sha, lufs, dur in con.execute('SELECT path, role, sha256, lufs, duration_s FROM assets'):
        stored[p] = dict(path=p, role=role, sha256=sha, lufs=lufs, duration_s=dur)
    merged = {**stored, **assets}
    gid, basis, edges = infer_groups(songs, rows, listed, merged)
    for t in todo:
        rows[t]['group_id'] = gid[t]
        rows[t]['group_basis'] = j(basis[t])
    with con:
        for a in assets.values():
            upsert(con, 'assets', ('path',), a)
        for t in todo:
            upsert(con, 'songs', ('id',), rows[t])
            con.execute('DELETE FROM label_counts WHERE song_id=?', (t,))
            for c in counts[t]:
                upsert(con, 'label_counts', ('song_id', 'family', 'file'), c)
            con.execute('DELETE FROM song_assets WHERE song_id=?', (t,))
            for p, via in listed[t]:
                con.execute('INSERT OR IGNORE INTO song_assets VALUES (?,?,?)', (t, p, via))
        con.execute('DELETE FROM songs WHERE id NOT IN (%s)' % ','.join('?' * len(songs)), list(songs))
        for t in gid:
            if t not in todo:
                con.execute('UPDATE songs SET group_id=?, group_basis=? WHERE id=?', (gid[t], j(basis[t]), t))
        con.execute('DELETE FROM group_edges')
        for (a, b, how), z in sorted(edges.items()):
            con.execute('INSERT INTO group_edges VALUES (?,?,?,?)', (a, b, how, z))
        sizes = {}
        for t, g in con.execute('SELECT id, COALESCE(group_override, group_id) FROM songs'):
            sizes[g] = sizes.get(g, 0) + 1
        for g, n in sizes.items():
            kind = 'generated' if g.startswith('gen_') else 'single' if n == 1 else 'versions'
            upsert(con, 'groups', ('id',), dict(id=g, kind=kind, size=n))
        con.execute('DELETE FROM groups WHERE id NOT IN (%s) AND note=\'\'' % ','.join('?' * len(sizes)), list(sizes))
        con.execute('DELETE FROM assets WHERE path NOT IN (SELECT path FROM song_assets) '
                    'AND path NOT IN (SELECT path FROM comp_assets)')
        seed_manual(con)
        ingest_remix(con)
    print(f'{len(todo)} songs, {len(assets)} files scanned')


def ingest_remix(con):
    """Unity checks and rendered remixes that tools/audio_analysis/lab/remix.py left in its cache."""
    d = Path.home() / '.cache/manifold/detector-lab/remix'
    for p in sorted((d / 'unity').glob('*.json')) if (d / 'unity').exists() else []:
        r = json.loads(p.read_text())
        upsert(con, 'checks', ('song_id', 'name'),
               dict(song_id=r['song'], name='unity', lag_s=r.get('lag_s'), lag_samples=r.get('lag_samples'),
                    corr=r.get('corr'), residual_db=r.get('residual_db'), raw_residual_db=r.get('raw_residual_db'),
                    gain_db=r.get('gain_db'), verdict=r['verdict'], detail=j(r.get('detail', {}))))
    for p in sorted((d / 'render').glob('*.json')) if (d / 'render').exists() else []:
        r = json.loads(p.read_text())
        upsert(con, 'renders', ('key',), dict(key=r['key'], song_id=r['song'], seed=r['seed'],
                                              recipe_version=r['recipe_version'], path=r['path'], sha256=r.get('sha256'),
                                              sources=j(r['sources']), params=j(r['params'])))


# ---- reading ------------------------------------------------------------------------------------------------------

REMIX_ROLE = {'snare': 'drums'}


def stems(song_id, db=None):
    """{role: [Path]} of the song's stem set, local files only. Roles: drums (incl. snare parts), kick, bass, other,
    nodrums, nokick. Empty when the song has no stem set."""
    con = connect(db or DB, readonly=True)
    row = con.execute('SELECT stem_set FROM songs WHERE id=?', (song_id,)).fetchone()
    ss = json.loads(row[0]) if row and row[0] else None
    if not ss:
        return {}
    out = {}
    for p in ss['paths']:
        role = con.execute('SELECT role FROM assets WHERE path=?', (p,)).fetchone()[0]
        out.setdefault(REMIX_ROLE.get(role, role), []).append(Path(p))
    return out


def stem_sha(path, db=None):
    con = connect(db or DB, readonly=True)
    row = con.execute('SELECT sha256 FROM assets WHERE path=?', (str(path),)).fetchone()
    return row[0] if row else None


def table(rows, head):
    w = [max(len(str(x)) for x in col) for col in zip(head, *rows)] if rows else [len(h) for h in head]
    for r in [head] + rows:
        print('  '.join(str(x).ljust(n) for x, n in zip(r, w)).rstrip())


def show(sid):
    con = connect(readonly=True)
    r = con.execute('SELECT * FROM songs WHERE id=?', (sid,)).fetchone()
    if not r:
        sys.exit(f'no song {sid}')
    d = dict(zip([c[0] for c in con.execute('SELECT * FROM songs').description], r))
    print(f"{d['id']}  ({d['title']})  {d['kind']}  rate {d['sample_rate']}  offset {d['offset_s']}  proof {d['proof']}")
    print(f"  content_id {d['content_id'][:16]}   families {d['families']}")
    grp = d['group_override'] or d['group_id']
    members = [x[0] for x in con.execute('SELECT id FROM songs WHERE COALESCE(group_override, group_id)=? ORDER BY id', (grp,))]
    print(f"  group {grp}{' (override)' if d['group_override'] else ''}  basis {d['group_basis']}  members {members}")
    print(f"  licence {d['licence_override'] or d['licence']}{' (override)' if d['licence_override'] else ''}: {d['licence_reason']}  {d['licence_flags']}")
    if d['notes']:
        print(f"  notes: {d['notes']}")
    lm = json.loads(d['lab_mix'] or 'null')
    print(f"  lab mix: {lm}")
    for c in con.execute('SELECT family,file,positives,hard_neg,unscored_s,builder FROM label_counts WHERE song_id=?', (sid,)):
        print('  labels', c)
    src = json.loads(d['source'])
    print('  project dirs', src['project_dirs'])
    for a in src['als']:
        print('  als', a['path'].replace(str(ABL) + '/', ''), a['mtime_ns'])
    ss = json.loads(d['stem_set'] or 'null')
    if ss:
        print(f"  stem set {ss['folder']}  files {len(ss['paths'])}  online-only {len(ss['online_only'])}")
    for c in con.execute('SELECT name,lag_s,corr,residual_db,raw_residual_db,gain_db,verdict FROM checks WHERE song_id=?', (sid,)):
        print('  check', c)
    rows = [[a[0], a[1], a[2] or '', 'local' if a[3] else 'ONLINE', a[4] or '', a[5] if a[5] is not None else '',
             a[6] if a[6] is not None else '', a[7] or '', a[8].replace(str(MUSIC) + '/', '')]
            for a in con.execute('SELECT sa.via, a.role, a.version, a.local, a.sha256, a.lufs, a.duration_s, a.sample_rate, a.path '
                                 'FROM song_assets sa JOIN assets a USING(path) WHERE sa.song_id=? ORDER BY a.role, a.path', (sid,))]
    table([[r[0], r[1], r[2], r[3], r[4][:8], r[5], r[6], r[7], r[8]] for r in rows],
          ['via', 'role', 'version', 'state', 'sha', 'LUFS', 'sec', 'sr', 'path'])


def find(args):
    con = connect(readonly=True)
    q = ('SELECT DISTINCT a.path, a.role, a.version, a.local, a.lufs, a.duration_s, s.id, s.kind, COALESCE(s.licence_override, s.licence) '
         'FROM assets a JOIN song_assets sa USING(path) JOIN songs s ON s.id=sa.song_id WHERE 1=1')
    p = []
    for col, val in (('a.role', args.role), ('s.kind', args.kind), ('COALESCE(s.licence_override, s.licence)', args.licence), ('s.id', args.song)):
        if val:
            q += f' AND {col}=?'
            p.append(val)
    if args.local:
        q += ' AND a.local=1'
    if args.online:
        q += ' AND a.local=0'
    q += ' ORDER BY s.id, a.role, a.path'
    rows = [[r[6], r[1], r[2] or '', 'local' if r[3] else 'ONLINE', r[4] if r[4] is not None else '', r[5] or '', r[8],
             r[0].replace(str(MUSIC) + '/', '')] for r in con.execute(q, p)]
    table(rows, ['song', 'role', 'version', 'state', 'LUFS', 'sec', 'licence', 'path'])
    print(f'{len(rows)} files')


def stats():
    con = connect(readonly=True)
    q = lambda s: con.execute(s).fetchall()  # noqa: E731
    print('songs', q('SELECT COUNT(*) FROM songs')[0][0], dict(q('SELECT kind, COUNT(*) FROM songs GROUP BY kind')))
    print('licence per song', dict(q('SELECT lic, COUNT(*) FROM song_groups GROUP BY lic')))
    print('files', q('SELECT COUNT(*) FROM assets')[0][0], ' local', q('SELECT COUNT(*) FROM assets WHERE local=1')[0][0],
          ' online-only', q('SELECT COUNT(*) FROM assets WHERE local=0')[0][0])
    print('files per role (local/online):')
    table([[r, l, o] for r, l, o in q('SELECT role, SUM(local=1), SUM(local=0) FROM assets GROUP BY role ORDER BY role')], ['role', 'local', 'online'])
    hrs = q('SELECT SUM(duration_s)/3600.0 FROM (SELECT DISTINCT sha256, duration_s FROM assets WHERE local=1 AND sha256 IS NOT NULL)')[0][0]
    print(f'unique local audio: {hrs:.1f} h')
    print('hours per role:', {r: round(h, 1) for r, h in q('SELECT role, SUM(duration_s)/3600.0 FROM (SELECT DISTINCT sha256, role, duration_s FROM assets WHERE local=1) GROUP BY role')})
    ready = q("SELECT s.id FROM songs s JOIN checks c ON c.song_id=s.id AND c.name='unity' WHERE c.verdict='usable' ORDER BY s.id")
    print(f'remix-ready songs ({len(ready)}):', ', '.join(r[0] for r in ready))
    mism = q("SELECT id FROM songs WHERE json_extract(lab_mix,'$.match')='none' ORDER BY id")
    print(f'lab mix matches no file on disk ({len(mism)}):', ', '.join(r[0] for r in mism))
    print('groups: ', q('SELECT COUNT(*) FROM groups')[0][0], ' with 2+ registry songs:')
    for g, n, ms in q("SELECT grp, COUNT(*), group_concat(song_id, ',') FROM song_groups GROUP BY grp HAVING COUNT(*)>1 ORDER BY grp"):
        print(f'  {g} ({n}): {ms}')


def dump():
    con = connect(readonly=True)
    for (name,) in con.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").fetchall():
        cols = [c[1] for c in con.execute(f'PRAGMA table_info({name})')]
        print(f'## {name}')
        for r in con.execute(f'SELECT * FROM {name} ORDER BY {", ".join(str(i + 1) for i in range(min(2, len(cols))))}'):
            print(' | '.join(f'{c}={v}' for c, v in zip(cols, r) if v is not None))


def sql(query):
    con = connect(readonly=True)
    cur = con.execute(query)
    print('\t'.join(c[0] for c in cur.description))
    for r in cur.fetchall():
        print('\t'.join('' if v is None else str(v) for v in r))


def whole_tree(a):
    """The whole-tree commands (tree.py, comps.py, variants.py, usable.py)."""
    if a.cmd == 'usable':
        from tools.audio_analysis.catalog import usable
        usable.report(connect(readonly=True), a)
        return
    con = connect()
    if a.cmd == 'scan-tree':
        from tools.audio_analysis.catalog import tree
        tree.scan(con, meta=not a.no_meta)
    elif a.cmd == 'scan-sets':
        from tools.audio_analysis.catalog import tree
        tree.scan_sets(con, tuple(a.only), a.force)
    elif a.cmd == 'comps':
        from tools.audio_analysis.catalog import comps, usable
        comps.build(con)
        usable.match_sets(con)
    elif a.cmd == 'download':
        from tools.audio_analysis.catalog import tree
        tree.download(con, a.paths, a.why)
    elif a.cmd in ('variants', 'stemsum'):
        from tools.audio_analysis.catalog import variants
        variants.run(con, a.cmd, a.songs, a.force)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    sub.add_parser('build').add_argument('songs', nargs='*')
    sub.add_parser('show').add_argument('song')
    f = sub.add_parser('find')
    f.add_argument('--role')
    f.add_argument('--kind')
    f.add_argument('--licence')
    f.add_argument('--song')
    f.add_argument('--local', action='store_true')
    f.add_argument('--online', action='store_true')
    sub.add_parser('stats')
    sub.add_parser('dump')
    sub.add_parser('sql').add_argument('query')
    t = sub.add_parser('scan-tree')
    t.add_argument('--no-meta', action='store_true')
    t = sub.add_parser('scan-sets')
    t.add_argument('only', nargs='*')
    t.add_argument('--force', action='store_true')
    sub.add_parser('comps')
    t = sub.add_parser('variants')
    t.add_argument('songs', nargs='*')
    t.add_argument('--force', action='store_true')
    t = sub.add_parser('stemsum')
    t.add_argument('songs', nargs='*')
    t.add_argument('--force', action='store_true')
    t = sub.add_parser('download')
    t.add_argument('paths', nargs='+')
    t.add_argument('--why', required=True)
    t = sub.add_parser('usable')
    t.add_argument('comp', nargs='?')
    t.add_argument('--kind')
    t.add_argument('--lab', action='store_true')
    t.add_argument('--ready', action='store_true')
    t.add_argument('--blocked', action='store_true')
    t.add_argument('--remix', action='store_true')
    a = ap.parse_args()
    if a.cmd == 'build':
        build(set(a.songs))
    elif a.cmd == 'show':
        show(a.song)
    elif a.cmd == 'find':
        find(a)
    elif a.cmd == 'stats':
        stats()
    elif a.cmd == 'dump':
        dump()
    elif a.cmd == 'sql':
        sql(a.query)
    else:
        whole_tree(a)


if __name__ == '__main__':
    main()
