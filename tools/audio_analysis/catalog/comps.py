#!/usr/bin/env python3
"""Compositions: every song, remix, stem pack and idea in the music folder as one row, whether the lab uses it or not.

A composition is what the file names say a file is about (names.main_title), with working titles unified: a bracketed
alias inside one name ("Imitation (Pusher)") joins the two titles, the registry's titles and aliases join (catalog.TITLES),
and ALIAS below holds the calls the names alone cannot make (a project called Changes that is the song Waiting For You).
A project folder whose own title names no file but whose files mostly name one song is that song (Path -> Running).
Files in a stem-like folder that name no known song belong to the folder's song. Every call is in `basis`.

Kinds: lab (a registry song), generated, fixture, original (Peter's own song outside the lab), remix (Peter's remix of
someone else's song: third-party parts inside), stem_pack (other people's stems, no remix project of Peter's),
infra (live-show, routing and test sets: no music). comps.kind_override is Peter's; a rebuild never touches it or note.

Usage: catalog.py comps"""
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from tools.audio_analysis.catalog import names  # noqa: E402
from tools.audio_analysis.catalog.names import norm  # noqa: E402

# normalised title -> composition id. Calls the file names do not make themselves.
ALIAS = {
    'changes': 'waiting_for_you', 'growth': 'all_in_for_you', 'allin4u': 'all_in_for_you',
    'limerent': 'set_me_free', 'limerence': 'set_me_free', 'limerant': 'set_me_free',
    'cold': 'cold_remix', 'eleonoracold': 'cold_remix', 'thebusiness': 'business', 'kx5': 'eat_sleep',
    'worshipmax': 'worship', 'tiestobusinessstemsselected': 'business', 'lhstems': 'lh',
    'cooldrums': 'mirage', 'oppression': 'opression', 'diysupergroup': 'ever_knew', 'diy': 'ever_knew', 'diyremix': 'ever_knew',
    'waypoints': 'waypoints', 'ways': 'waypoints', 'sense': 'touch', 'path': 'running', 'map': 'paces', 'bright': 'go_with_you',
    'rise': 'control', 'clue': 'echo', 'idle': 'dream', 'wait': 'desire', 'stay': 'skyline', 'faux': 'concentrate',
    'experiment': 'yesterday', 'pusher': 'imitation', 'tension': 'rush', 'whathaveidone': 'gerrit', 'gerritmixdown': 'gerrit',
    'gerritmaster': 'gerrit', 'citizenoftheworld': 'gerrit', 'oliverheldensheartcry': 'heart_cry', 'heartcry': 'heart_cry',
    'katsudon': 'corrosion', 'mudpie': 'got_it', 'iguessitsjustthis': 'lowkey', 'lowkey': 'lowkey',
    'allineed': 'all_i_need', 'moveroom': 'move_room', 'texture': 'heavy_on_mind', 'streets': 'miracle',
    'bettys': 'midnight_patience', 'states': 'back_to_you', 'fifty': 'burn_stems', 'burn': 'burn_stems',
    'snackpack': 'murmur', 'steady': 'right_where', 'rainfall': 'hung_up', 'downpour': 'hung_up', 'sounddesignsession': 'know_youre_there',
    'knowyouretherexzoiia': 'know_youre_there', 'stripped': 'miracle', 'miraclestripped': 'miracle',
}
# (normalised title, path prefix under the music folder) -> composition id: same name, different song.
SCOPED = {('growth', 'Ableton Projects/Ableton 11/'): 'growth_2023', ('renew', 'Ableton Projects/Ableton 11/'): 'renew_2023',
          ('openings', 'Ableton Projects/Ableton 11/'): 'openings_2023', ('dusk2026', ''): 'dusk2026'}
INFRA = re.compile(r'live show|osc |^osc|resolume|video|blender|xone|live template|live setup|default template|midi mess|tidal|'
                   r'premaster checks|mix testing|^media$|stem split|labels and timings|linndrum|bedroom set|^project$|'
                   r'^album mix$|^osc only$|josh quick|^general$|^home$|^launch$|^continue$|^noise$|^one$|^rest$|^time$', re.I)
INFRA_PATH = re.compile(r'Blender Tests|Live Show|LIve Show|SMPTE|Resolume|Video Sequences|Labels and Timings|LinnDrum|Bedroom Set', re.I)
REMIX_WORD =re.compile(r'remix|\bedit\b|dnb edit|\bk ?i ?e\b', re.I)
STEM_LIKE = re.compile(r'stem|groups|tayla vox|dry|wet|export test|video stems|blender stems', re.I)
PACK_ROOT = 'Old Albums Remix Stems And Extras/Remix Stems'


ALIAS_TITLES = {'know_youre_there': "Know You're There", 'all_in_for_you': 'All In For You', 'waiting_for_you': 'Waiting For You',
                'head_noise': 'Head Noise', 'go_with_you': 'Go With You', 'heart_cry': 'Heart Cry', 'ever_knew': 'Ever Knew',
                'all_i_need': 'All I Need', 'opression': 'Opression'}
PAREN_NO_JOIN = {'beefrendang'}       # a working title shared by two songs (Head Noise and Oppression were both "Beef Rendang")


def slug(s):
    return re.sub(r'[^a-z0-9]+', '_', s.lower().replace("'", '').replace('’', '')).strip('_')


def registry_ids(con):
    """{norm title or alias: group id} and {group id: title} from the registry songs."""
    from tools.audio_analysis.catalog import catalog
    alias, title, members = {}, {}, {}
    rows = con.execute('SELECT id, title, kind, COALESCE(group_override, group_id) FROM songs').fetchall()
    for sid, ttl, kind, grp in rows:
        members.setdefault(grp, []).append(sid)
        if kind != 'project':
            title.setdefault(grp, ttl)
            continue
        names_ = catalog.TITLES.get(sid, (ttl, []))
        title.setdefault(grp, names_[0])
        for n in [names_[0]] + list(names_[1]) + [sid]:
            if len(norm(n)) >= 3:
                alias[norm(n)] = grp
    return alias, title, members


class Union:
    def __init__(self):
        self.p = {}

    def find(self, x):
        self.p.setdefault(x, x)
        while self.p[x] != x:
            self.p[x] = self.p[self.p[x]]
            x = self.p[x]
        return x

    def join(self, a, b):
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            self.p[rb] = ra


def rel(path, music):
    return os.path.relpath(path, music)


def project_of(relpath):
    parts = relpath.split(os.sep)
    for i in range(len(parts) - 2, 0, -1):
        if parts[i].endswith(' Project') or parts[i] in ('Oprression', 'Jordi Acoustic Edit'):
            return os.sep.join(parts[:i + 1]), parts[i]
    return None, None


class Resolver:
    def __init__(self, reg_alias):
        self.alias = {**{k: v for k, v in ALIAS.items()}, **reg_alias}
        self.u = Union()
        self.basis = {}
        self.display = {}

    def canon(self, title, relpath=''):
        k = norm(title)
        for (sk, prefix), cid in SCOPED.items():
            if k == sk and relpath.startswith(prefix):
                return cid
        return self.alias.get(k) or slug(title) or k

    def note(self, cid, how, title=None):
        self.basis.setdefault(cid, set()).add(how)
        if title:
            self.display.setdefault(cid, {}).setdefault(title, 0)
            self.display[cid][title] += 1


def comp_of_file(res, relpath, known, project_title):
    """(composition id, how, title shown) for an audio file by its name, its folders and its project. In order: a pack
    folder of stems; a file name that is a known song, or a bounce (WIP/FINAL/MASTER...) outside a stem folder; a folder
    named for a song; the project's song; the file name."""
    parts = relpath.split(os.sep)
    name, parents = parts[-1], parts[:-1]
    root = PACK_ROOT.split(os.sep)
    if relpath.startswith(PACK_ROOT + os.sep) and len(parents) > len(root):
        pack = parents[len(root)]
        if 'pack' in pack.lower() and len(parents) > len(root) + 1:
            pack = parents[len(root) + 1]            # a pack of several artists' stems: one song per artist folder
        t, _ = names.main_title(pack)
        return res.canon(t or pack, relpath), 'pack folder', t or pack
    if 'Labels and Timings Project' in parents:
        return 'labels_and_timings', 'project name', 'Labels and Timings'
    title, aliases = names.main_title(name)
    stemlike = any(STEM_LIKE.search(p) for p in parents[1:])
    named = title and not names.generic(title)
    if named and (norm(title) in known or (not stemlike and (names.versioned(name) or not project_title))):
        return res.canon(title, relpath), 'file name', title
    for p in reversed(parents[1:]):
        ft = names.folder_title([p])
        if ft and (norm(ft) in known or 'stem' in p.lower()):
            return res.canon(ft, relpath), 'folder name', ft
    if project_title:
        return res.canon(project_title, relpath), 'project name', project_title
    if title:
        return res.canon(title, relpath), 'file name', title
    return None, 'none', ''


def infer(con, music=None):
    from tools.audio_analysis.catalog import catalog
    music = str(music or catalog.MUSIC)
    reg_alias, reg_title, members = registry_ids(con)
    res = Resolver(reg_alias)
    skipped = [r[0] + os.sep for r in con.execute('SELECT path FROM skipped')]
    assets = [r for r in con.execute("SELECT path FROM assets WHERE origin='music' AND path LIKE ?", (music + '/%',))
              if not any(r[0].startswith(s) for s in skipped)]
    sets = con.execute('SELECT path, project_dir FROM als_files').fetchall()
    # titles the sets and project folders name: a file in a stem-like folder is "known" if its title is among them
    known = set(reg_alias)
    proj_title = {}
    for (p,) in assets:
        pr, pn = project_of(rel(p, music))
        if pr and pr not in proj_title:
            proj_title[pr] = names.main_title(re.sub(r' Project$', '', pn))[0]
    for pth, pdir in sets:
        t, al = names.main_title(os.path.basename(pth))
        for x in [t] + al:
            if x:
                known.add(norm(x))
    for pr, t in list(proj_title.items()):
        known.add(norm(t))
    known |= {k for k in ALIAS}
    file_comp, set_comp = {}, {}
    for (p,) in assets:
        r = rel(p, music)
        _, pn = project_of(r)
        pt = proj_title.get(project_of(r)[0]) if pn else None
        cid, how, shown = comp_of_file(res, r, known, pt)
        if not cid:
            continue
        title, aliases = names.main_title(os.path.basename(p))
        res.note(cid, how, shown)
        for a in aliases if how == 'file name' else ():
            if norm(a) not in PAREN_NO_JOIN:
                res.u.join(cid, res.canon(a, r))
                res.note(cid, 'working title ' + a)
        file_comp[p] = (cid, how)
    for pth, pdir in sets:
        r = rel(pth, music)
        title, aliases = names.main_title(os.path.basename(pth))
        if not title or names.generic(title):
            title = names.main_title(re.sub(r' Project$', '', os.path.basename(pdir)))[0]
        if not title:                      # a set named only with stop words (Album Mix, OSC Only): its own name
            title = re.sub(r'\.als$', '', os.path.basename(pth), flags=re.I)
        cid = res.canon(title, r)
        res.note(cid, 'set name', title)
        for a in aliases:
            res.u.join(cid, res.canon(a, r))
            res.note(cid, 'working title ' + a)
        set_comp[pth] = cid
    # a project folder whose own title names no file but whose files mostly name one song is that song
    by_proj = {}
    for p, (cid, how) in file_comp.items():
        pr, _ = project_of(rel(p, music))
        if pr and not os.path.basename(p).lower().endswith(('.asd',)):
            by_proj.setdefault(pr, {}).setdefault(res.u.find(cid), 0)
            by_proj[pr][res.u.find(cid)] += 1
    for pr, counts in by_proj.items():
        own = res.u.find(res.canon(proj_title.get(pr, ''), pr))
        if proj_title.get(pr) and own not in counts:
            top = max(counts, key=counts.get)
            if counts[top] >= 2:
                res.u.join(top, own)
                res.note(top, f'project folder {Path(pr).name} holds this song')
    # final ids: a registry group id when a member is one, else the most used title's slug
    roots = {}
    for cid in list(res.basis) + [c for c, _ in file_comp.values()] + list(set_comp.values()):
        roots.setdefault(res.u.find(cid), set()).add(cid)
    final = {}
    targets = set(ALIAS.values()) | set(SCOPED.values())
    for root, cids in roots.items():
        lab = sorted(c for c in cids if c in members or c in reg_title) or sorted(c for c in cids if c in targets)
        final[root] = lab[0] if lab else None
    for root, cids in roots.items():
        if final[root] is None:
            shown = {}
            for c in cids:
                for t, n in res.display.get(c, {}).items():
                    shown[t] = shown.get(t, 0) + n
            best = max(shown, key=shown.get) if shown else root
            final[root] = slug(best) or root
    fid = lambda c: final[res.u.find(c)]  # noqa: E731
    return dict(file_comp={p: fid(c) for p, (c, h) in file_comp.items()}, how={p: h for p, (c, h) in file_comp.items()},
                set_comp={p: fid(c) for p, c in set_comp.items()}, res=res, members=members, reg_title=reg_title, final=final,
                fid=fid)


def kind_of(cid, files, sets, members, music, info):
    """lab | generated | fixture | original | remix | stem_pack | infra for one composition."""
    from tools.audio_analysis.catalog import catalog
    if cid in members:
        ms = members[cid]
        if cid.startswith('gen_'):
            return 'generated'
        if all(m in catalog.FIXTURES for m in ms):
            return 'fixture'
        return 'lab'
    rels = [rel(p, music) for p in files] + [rel(p, music) for p in sets]
    title = info['title']
    if INFRA.search(title) or (rels and all(INFRA_PATH.search(r) for r in rels)):
        return 'infra'
    if rels and all(r.startswith(PACK_ROOT + os.sep) for r in rels):
        return 'stem_pack'
    if any(r.startswith('Ableton Projects/Remixs/') for r in rels) or REMIX_WORD.search(title):
        return 'remix'
    return 'original'


def build(con, music=None):
    from tools.audio_analysis.catalog import catalog
    from tools.audio_analysis.catalog.tree import REFERENCE
    music = str(music or catalog.MUSIC)
    info = infer(con, music)
    res = info['res']
    comps = {}
    for p, cid in info['file_comp'].items():
        comps.setdefault(cid, dict(files=[], sets=[]))['files'].append(p)
    for p, cid in info['set_comp'].items():
        comps.setdefault(cid, dict(files=[], sets=[]))['sets'].append(p)
    for cid in info['members']:
        comps.setdefault(cid, dict(files=[], sets=[]))
    disp, basis_of = {}, {}
    for raw in set(res.display) | set(res.basis):
        f = info['fid'](raw)
        for t, n in res.display.get(raw, {}).items():
            disp.setdefault(f, {})[t] = disp.get(f, {}).get(t, 0) + n
        basis_of.setdefault(f, set()).update(res.basis.get(raw, ()))
    rows = []
    for cid, d in comps.items():
        shown = disp.get(cid, {})
        title = (info['reg_title'].get(cid) or ALIAS_TITLES.get(cid) or (max(shown, key=shown.get) if shown else cid))
        kind = kind_of(cid, d['files'], d['sets'], info['members'], music, dict(title=title))
        if kind == 'original' and d['files'] and not d['sets'] and all(REFERENCE.search(os.path.basename(p)) for p in d['files']):
            kind = 'reference'
        rows.append((cid, title, kind, sorted(basis_of.get(cid, ())), sorted(shown, key=shown.get, reverse=True)[:6],
                     info['members'].get(cid, [])))
    with con:
        con.execute('DELETE FROM comp_assets')
        con.execute('DELETE FROM comp_als')
        for cid, title, kind, basis, aliases, grp in rows:
            catalog.upsert(con, 'comps', ('id',), dict(id=cid, title=title, kind=kind, basis=catalog.j(basis),
                                                       aliases=catalog.j(aliases), groups=catalog.j(grp)))
        con.execute('DELETE FROM comps WHERE id NOT IN (%s)' % ','.join('?' * len(rows)), [r[0] for r in rows])
        for p, cid in info['file_comp'].items():
            flags = ['reference'] if REFERENCE.search(os.path.basename(p)) else []
            con.execute('INSERT OR REPLACE INTO comp_assets VALUES (?,?,?,?)', (cid, p, info['how'][p], catalog.j(flags)))
        for p, cid in info['set_comp'].items():
            con.execute('INSERT OR REPLACE INTO comp_als VALUES (?,?)', (cid, p))
        # registry files the tree walk did not place stay with their song's composition
        placed = set(info['file_comp'])
        skipped = [r[0] + os.sep for r in con.execute('SELECT path FROM skipped')]
        for sid, kind, p, via in con.execute('SELECT sa.song_id, s.kind, sa.path, sa.via FROM song_assets sa JOIN songs s ON s.id=sa.song_id'):
            if any(p.startswith(s) for s in skipped):
                continue
            grp = con.execute('SELECT COALESCE(group_override, group_id) FROM songs WHERE id=?', (sid,)).fetchone()[0]
            if kind == 'generated':        # the registry's own export files: not the infra composition that holds the project
                con.execute('DELETE FROM comp_assets WHERE path=? AND comp_id NOT LIKE ?', (p, 'gen_%'))
                con.execute('INSERT OR IGNORE INTO comp_assets VALUES (?,?,?,?)', (grp, p, 'registry: ' + via, '[]'))
            elif p not in placed:
                con.execute('INSERT OR IGNORE INTO comp_assets VALUES (?,?,?,?)', (grp, p, 'registry: ' + via, '[]'))
    print(f'{len(rows)} compositions')
    return rows
