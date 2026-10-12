#!/usr/bin/env python3
"""What training material each composition has, in one view.

Per composition, from the catalogue's tables only (nothing here opens audio or a set):
  lab         a registry song with labels (families and positive counts)
  variants    its other bounces judged against the lab mix: same_arrangement (labels transfer), needs_check, different
  remix       a stem set that sums to a bounce (checks.unity usable, or a stem folder that sums to a bounce in `variants`)
  label_ready a non-lab composition whose Ableton set has MIN_NOTES+ kick or snare/clap MIDI notes and has audio
  blocked     why not, in one phrase

Usage: catalog.py usable [--kind K] [--lab] [--ready] [--blocked] [--remix] | usable COMP"""
import json
import os
import re

from tools.audio_analysis.catalog import als_scan

STEM_ROLES = ('drums', 'kick', 'snare', 'bass', 'other', 'nodrums', 'nokick')
BOUNCE_ROLES = ('full_mix', 'master', 'premaster')
VERSION_WORD = re.compile(r'wip ?\d+|final ?\d*|premaster|master|mstr|t[1-3]\b|v\d+|mixdown|album|live|edits?|remaster|export', re.I)
MUSIC_KINDS = ('lab', 'original', 'remix', 'stem_pack')


def name_tokens(s):
    s = re.sub(r'\.(als|wav|mp3|aiff?|flac)$', '', s, flags=re.I)
    return {t.lower().replace(' ', '') for t in VERSION_WORD.findall(s)}


def match_sets(con):
    """For every bounce of every composition, the Ableton set that probably made it: sets whose arrangement length is
    within 3% of the bounce first; among those the most version words in common (PREMASTER T2, FINAL, MSTR V3...), then
    the nearest earlier save. A guess from names, lengths and dates, recorded with how it was made; the label code's own
    set for a lab song is in songs.source."""
    con.execute('DELETE FROM als_matches')
    for cid, in con.execute("SELECT id FROM comps WHERE kind IN ('lab','original','remix')").fetchall():
        sets = con.execute('SELECT a.path, a.name, a.mtime_ns, a.arrangement_s FROM comp_als c JOIN als_files a USING(path) '
                           'WHERE c.comp_id=?', (cid,)).fetchall()
        if not sets:
            continue
        for p, name, mtime, dur, role in con.execute(
                "SELECT a.path, a.name, a.mtime_ns, a.duration_s, a.role FROM comp_assets ca JOIN assets a USING(path) "
                "WHERE ca.comp_id=? AND a.role IN ('full_mix','master','premaster')", (cid,)).fetchall():
            best, key = None, None
            for sp, sname, smt, arr in sets:
                diff = abs(arr - dur) if arr and dur else None
                tok = len(name_tokens(name) & name_tokens(sname))
                gap = (mtime - smt) / 86400e9
                k = (0 if diff is not None and diff <= .03 * dur else 1, -tok, abs(gap) if gap < 0 else gap)
                if key is None or k < key:
                    best, key = (sp, diff, gap, tok), k
            sp, diff, gap, tok = best
            how = f"{'length' if key[0] == 0 else 'no length match'}, {tok} shared version words, set saved {gap:.1f} days before"
            con.execute('INSERT OR REPLACE INTO als_matches VALUES (?,?,?,?,?)', (p, sp, how, gap, diff))
    con.commit()


def stem_folders(con, comp):
    """Folders of the composition holding a stem set: 2+ local files of stem roles with matching lengths.
    [(folder, [(path, role, duration)])]."""
    by = {}
    for p, role, dur, local in con.execute(
            "SELECT a.path, a.role, a.duration_s, a.local FROM comp_assets ca JOIN assets a USING(path) WHERE ca.comp_id=?", (comp,)):
        if role in STEM_ROLES:
            by.setdefault(os.path.dirname(p), []).append((p, role, dur, local))
    return [(d, fs) for d, fs in sorted(by.items()) if len(fs) >= 2]


def comp_summary(con, cid):
    """Everything the usable view says about one composition."""
    title, kind = con.execute('SELECT title, COALESCE(kind_override, kind) FROM comps WHERE id=?', (cid,)).fetchone()
    s = dict(id=cid, title=title, kind=kind)
    songs = [r[0] for r in con.execute("SELECT id FROM songs WHERE COALESCE(group_override, group_id)=? AND kind='project'", (cid,))]
    s['songs'] = songs
    s['labels'] = {}
    for sid in songs:
        for fam, pos in con.execute('SELECT family, SUM(positives) FROM label_counts WHERE song_id=? GROUP BY family', (sid,)):
            s['labels'].setdefault(sid, {})[fam] = pos
    v = {}
    s['same'], s['identical'], seen = [], 0, set()
    q = ("SELECT v.song_id, v.path, v.verdict, v.detail, a.sha256 FROM variants v LEFT JOIN assets a USING(path) "
         "WHERE v.ref='lab_mix' AND v.song_id IN (%s)" % ','.join('?' * len(songs))) if songs else None
    for sid, path, verdict, detail, sha in (con.execute(q, songs) if q else ()):
        d = json.loads(detail or '{}')
        if d.get('identical'):
            s['identical'] += 1                  # the lab mix itself under another name: not a new variant
            continue
        if (sid, sha or path) in seen:
            continue                              # the same bytes in two folders count once
        seen.add((sid, sha or path))
        v[verdict] = v.get(verdict, 0) + 1
        if verdict == 'same_arrangement':
            s['same'].append((sid, os.path.basename(path)))
    s['variants'] = v
    s['stem_sums'] = []
    for ref, path, verdict, detail, res in con.execute(
            "SELECT ref, path, verdict, detail, residual_db FROM variants WHERE ref LIKE 'stem_sum:%' AND song_id=?", (cid,)):
        d = json.loads(detail or '{}')
        s['stem_sums'].append(dict(set=ref.split(':', 1)[1], bounce=os.path.basename(path), verdict=verdict, residual_db=res,
                                   sums=bool(d.get('sums'))))
    s['generated'] = None
    for sid, g in con.execute("SELECT id, generated FROM songs WHERE COALESCE(group_override, group_id)=? AND generated IS NOT NULL", (cid,)):
        g = json.loads(g)
        s['generated'] = dict(song=sid, export=os.path.basename(g.get('export') or ''), project=os.path.basename(g.get('project_dir') or ''),
                              sets=[os.path.basename(a['path']) for a in g.get('als', [])])
        break
    s['unity'] = {sid: con.execute("SELECT verdict FROM checks WHERE song_id=? AND name='unity'", (sid,)).fetchone() for sid in songs}
    s['unity'] = {k: x[0] for k, x in s['unity'].items() if x}
    roles = {}
    for role, local, n in con.execute("SELECT a.role, a.local, COUNT(*) FROM comp_assets ca JOIN assets a USING(path) WHERE ca.comp_id=? "
                                      "AND ca.flags NOT LIKE '%reference%' GROUP BY a.role, a.local", (cid,)):
        roles.setdefault(role, [0, 0])[0 if local else 1] += n
    s['audio'] = roles
    s['bounces_local'] = sum(roles.get(r, [0, 0])[0] for r in BOUNCE_ROLES)
    s['bounces_online'] = sum(roles.get(r, [0, 0])[1] for r in BOUNCE_ROLES)
    s['stems_local'] = sum(roles.get(r, [0, 0])[0] for r in STEM_ROLES)
    s['stems_online'] = sum(roles.get(r, [0, 0])[1] for r in STEM_ROLES)
    sets = con.execute('SELECT a.path, a.name, a.local, a.verdict, a.kick_notes, a.snare_notes, a.audio_drums, a.mtime_ns '
                       'FROM comp_als c JOIN als_files a USING(path) WHERE c.comp_id=?', (cid,)).fetchall()
    s['sets'] = len(sets)
    s['sets_online'] = sum(1 for r in sets if not r[2])
    ready = [r for r in sets if r[3] == 'ready']
    s['ready_sets'] = len(ready)
    best = max(ready, key=lambda r: (r[4] or 0) + (r[5] or 0), default=None)
    s['best_set'] = best and dict(name=best[1], kick=best[4], snare=best[5])
    s['audio_only_sets'] = sum(1 for r in sets if r[3] == 'audio_only')
    s['muted_sets'] = sum(1 for r in sets if r[3] == 'muted_only')
    s['unread_sets'] = sum(1 for r in sets if r[3] is None)
    s['remix_ready'] = (any(x == 'usable' for x in s['unity'].values()) or any(x['sums'] for x in s['stem_sums']))
    return s


def classify(s):
    """(state, why): lab | label_ready | blocked | out of scope, and a phrase."""
    k = s['kind']
    if k in ('infra', 'reference'):
        return 'out of scope', k
    if k in ('generated', 'fixture'):
        return 'lab', k
    if s['songs'] and s['labels']:
        return 'lab', 'labelled'
    audio = s['bounces_local'] + s['bounces_online'] + s['stems_local'] + s['stems_online']
    if k == 'stem_pack':
        return 'blocked', "other people's stems, no remix project of Peter's"
    if not audio:
        return 'blocked', 'no audio bounce or stems'
    if s['ready_sets']:
        if s['bounces_local'] or s['stems_local']:
            return 'label_ready', 'drum MIDI + local audio'
        return 'label_ready_online', 'drum MIDI; every bounce is online-only'
    if not s['sets']:
        return 'blocked', 'audio but no Ableton set'
    if s['audio_only_sets']:
        why = 'drums are audio clips (needs different handling)'
    elif s['muted_sets']:
        why = 'drum MIDI only on muted tracks'
    elif s['unread_sets'] == s['sets']:
        why = 'Ableton sets online-only (not read)'
    else:
        why = 'no kick/snare MIDI in the sets'
    if s['unread_sets'] and s['unread_sets'] < s['sets']:
        why += f"; {s['unread_sets']} more sets online-only (unread)"
    return 'blocked', why


def table(rows, head):
    w = [max(len(str(x)) for x in col) for col in zip(head, *rows)] if rows else [len(h) for h in head]
    for r in [head] + rows:
        print('  '.join(str(x).ljust(n) for x, n in zip(r, w)).rstrip())


def report(con, args):
    rows, state = [], {}
    for (cid,) in con.execute('SELECT id FROM comps ORDER BY id'):
        s = comp_summary(con, cid)
        st, why = classify(s)
        state[cid] = (s, st, why)
    if args.comp:
        s, st, why = state[args.comp]
        print(json.dumps(dict(s, state=st, why=why), indent=1, default=str))
        return
    for cid, (s, st, why) in state.items():
        if args.kind and s['kind'] != args.kind:
            continue
        if args.lab and st != 'lab':
            continue
        if args.ready and not st.startswith('label_ready'):
            continue
        if args.blocked and st != 'blocked':
            continue
        if args.remix and not s['remix_ready']:
            continue
        if s['kind'] in ('infra', 'reference') and not args.kind:
            continue
        v = s['variants']
        rows.append([cid, s['kind'], st,
                     ','.join(f"{f}:{n}" for sid, d in s['labels'].items() for f, n in d.items())[:24] if s['labels'] else '',
                     f"{v.get('same_arrangement', 0)}/{v.get('aligned_needs_check', 0)}/{v.get('different_arrangement', 0)}" if v else '',
                     'remix' if s['remix_ready'] else ('/'.join(f'{x}' for x in s['unity'].values()) if s['unity'] else ''),
                     f"{s['bounces_local']}+{s['bounces_online']}", f"{s['stems_local']}+{s['stems_online']}",
                     f"{s['ready_sets']}/{s['sets']}" + (f" ({s['best_set']['kick']}k {s['best_set']['snare']}s)" if s['best_set'] else ''),
                     why])
    table(rows, ['composition', 'kind', 'state', 'labels', 'variants s/c/d', 'stems sum', 'bounces', 'stems', 'ready sets', 'note'])
    counts = {}
    for cid, (s, st, why) in state.items():
        if s['kind'] not in ('infra', 'reference'):
            counts[st] = counts.get(st, 0) + 1
    print(f"\n{sum(counts.values())} compositions (infra and references excluded): {counts}")
