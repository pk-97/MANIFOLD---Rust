#!/usr/bin/env python3
"""Composition names from file, folder and set names: the title a name is about, with its working-title aliases.

main_title("Latent Space - 04 Midnight Patience T2 (mastered) 48k 24-bit.wav") -> ('Midnight Patience', [])
main_title("Imitation (Pusher) - PREMASTER T2.als")                           -> ('Imitation', ['Pusher'])
main_title("Kx5 - Eat Sleep (LATENT SPACE Remix) [PREMASTER].wav")            -> ('Eat Sleep', [])

Rules: brackets and the file extension go; a parenthesis holds a working title (an alias) unless it names a feature,
a mastering note or a remixer; segments that are numbers or the artist credit are dropped, the first segment left is
the title; version words, numbers and mastering words at the end of the title are stripped.
A name whose title is only an instrument word ("DRUMS", "Bass and Sub") has no title of its own: generic(key) is true
and the caller falls back to the folder."""
import re

AUDIO_EXT = ('.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a', '.als')
ARTIST = re.compile(r'latent space|^ls\b|east esplanade|^ee$|^k\.?\s?i\.?\s?e\.?$|binding|^#diy', re.I)
PAREN_SKIP = re.compile(r'^(feat|ft|mastered|k\.? ?i\.? ?e\b|latent|.*\bremix\b|.*\bedit\b|.*\bmix\b|draft|freeze|.*conflicted|.*copy|'
                        r'original|dry|wet|\[|\d+$|suite|pk |ls |car\b|studio)', re.I)
STOP = {'remix', 'wip', 'final', 'finals', 'album', 'premaster', 'premasters', 'pre', 'master', 'masters', 'mstr', 'aprs', 'mastered',
        'remaster', 'mixdown', 'mix', 'export', 'exports', 'edit', 'edits', 'resave', 'update', 'new', 'studio', 'beta',
        'sonible', 'bounce', 'flat', 'width', 'vsx', 'single', 'instrumental', 'no', 'stem', 'stems', 'ref', 'comp',
        'submission', 'bit', 'pk', 'eq', 'patched', 'patch', 'copy', 'conflicted', 'v', 't', 'maybe', 'draft', 'idea',
        'concept', 'demo', 'collab', 'prerelease', 'release', 'trim', 'version', 'slowdown', 'timmy', 'greedy',
        'liveshow', 'flatten', 'atmos', 'dess', 'apr', 'feedback', 'spatial', 'suite', 'headphones', 'earbuds', 'resample',
        'rebuild', 'wav', 'mp3', 'flac', 'aiff', 'only', 'short', 'slow', 'vox', 'osc', 'm4l', 'variation', 'layout',
        'alternate', 'soundcloud', 'reference', 'chill'}
NUMERIC = re.compile(r'^(?:v?\d+(?:k|_\d+|-\d+|(?:\.\d+)+)?|t\d|v\d+|\d+bit|\d+-bit|\d+k)$', re.I)
INSTR = {'drums', 'drum', 'kick', 'kicks', 'bass', 'sub', 'subs', 'chords', 'chord', 'fx', 'leads', 'lead', 'vox', 'vocal',
         'vocals', 'pad', 'pads', 'piano', 'pianos', 'synth', 'synths', 'all', 'and', 'other', 'main', 'bg', 'build',
         'snare', 'hats', 'perc', 'percussion', 'arp', 'stem', 'stems', 'audio', 'mix', 'master', 'premaster', 'full', 'no',
         'second', 'verse', 'half', 'intro', 'drop', 'scream', 'riser', 'sends', 'send', 'impact', 'laser', 'melodics',
         'wonky', 'fun', 'choir', 'tayla', 'rides', 'ride', 'off', 'granulator', 'radio', 'delay'}
FOLDER_GENERIC = {'finals', 'final', 'masters', 'master', 'stems', 'stem', 'wip', 'wips', 'album', 'albummix', 'albummixdown',
                  'albumfinals', 'premasters', 'premaster', 'premastert2', 'premasterst2', 'premasterst3', 'newstudio', 'old',
                  'exports', 'export', 'alexmasters', 'alexmaster', 'alext1', 'alext2', 'alext3', 'finalmasters', 'prerelease',
                  'finalrelease', '32bit', 'tayla vox', 'taylavox', 'part2', 'patch', 'groups', 'withkickstem', 'exporttest',
                  'finalmaybe', 'blenderstems', 'videostems', 'visuals', 'dry stems', 'drystems'}


def norm(s):
    return re.sub(r'[^a-z0-9]', '', s.lower())


def _strip_ext(s):
    low = s.lower()
    for e in AUDIO_EXT:
        if low.endswith(e):
            s = s[:-len(e)]
            low = s.lower()
    return s


def _tokens(s):
    return [t for t in re.split(r'[\s_]+', s.replace('’', "'")) if t]


def _is_stop(tok):
    t = tok.lower().strip('.,;:!?()[]+')
    return t in STOP or bool(NUMERIC.match(t)) or t in ('x', '&', '-', '+')


TRACK_NO = re.compile(r'0\d|1\d|2[0-4]')


def clean_title(seg, after_artist=False):
    """Strip a leading track number and trailing version/mastering words from one title segment. A number is a track
    number when it is 01-24 with zero padding or follows the artist credit; "48 Hours" keeps its number."""
    toks = _tokens(seg)
    while len(toks) > 1 and (toks[0] in ('-', '+') or re.fullmatch(r'\d+[.\-]?', toks[0]) and
                             (TRACK_NO.fullmatch(toks[0].strip('.-')) and toks[0][0] == '0' or after_artist)):
        toks.pop(0)
    parts = re.split(r'\s+x\s+', ' '.join(toks), maxsplit=1, flags=re.I)
    toks = _tokens(parts[0]) if parts[0] else toks
    while len(toks) > 1 and _is_stop(toks[-1]):
        toks.pop()
    return ' '.join(toks).strip(' -.')


def main_title(text):
    """(title, [alias titles]) for a file, folder or set name. The title is empty when nothing is left."""
    s = _strip_ext(text)
    s = re.sub(r'\.asd(\.\w+)?$', '', s)
    s = re.sub(r'\[[^\]]*\]', ' ', s)
    s = re.sub(r'\b\d{4}[-_]\d{2}[-_]\d{2}[ _]?\d*\b', ' ', s)
    parens = re.findall(r'\(([^)]*)\)', s) + re.findall(r'\(([^)]*)$', s)
    main = re.sub(r'\([^)]*\)?', ' ', s)
    main = re.sub(r'(\w)_re\b', r"\1're", main).replace('_', ' ')
    segs = [x.strip() for x in re.split(r'\s+-\s+|\s+–\s+', main) if x.strip()]
    keep = [x for x in segs if not re.fullmatch(r'\d+', x) and not ARTIST.search(x)]
    artist = any(ARTIST.search(x) for x in segs)
    remix = any(re.search(r'remix|edit|original mix|extended mix|radio mix', p, re.I) for p in parens)
    pick = None
    for i, seg in enumerate(keep):
        t = clean_title(seg, artist)
        if t and not all(_is_stop(w) for w in _tokens(t)):
            pick = t
            if remix and i == 0 and len(keep) > 1 and not _versionish(keep[1]):
                pick = clean_title(keep[1], artist) or t
            break
    title = pick or ''
    aliases = []
    for p in parens:
        if PAREN_SKIP.match(p.strip()):
            continue
        sub, _ = main_title(p) if ' - ' in p else (clean_title(p), [])
        if sub and norm(sub) != norm(title) and len(norm(sub)) >= 3:
            aliases.append(sub)
    return title, aliases


def _versionish(seg):
    return all(_is_stop(w) for w in _tokens(seg))


VERSIONED = re.compile(r'wip|final|master|mstr|mixdown|\bv\d|\bt\d\b|\bmix\b|bounce|remaster|export|edit', re.I)


def versioned(name):
    """A file name that marks a bounce of a song (WIP 4, FINAL, MASTER, V2, mixdown) rather than a part or a sample."""
    return bool(VERSIONED.search(_strip_ext(name)))


def generic(title):
    """True when a title is only instrument/part words (a stem called "Bass and Sub"): the folder names the song."""
    toks = [t.lower().strip('.,') for t in _tokens(title)]
    return not toks or all(t in INSTR or NUMERIC.match(t) for t in toks)


def folder_title(parents):
    """The title of the nearest folder that names a song (not "FINALS", "STEMS"...), deepest first, else ''."""
    for p in reversed(parents):
        key = norm(p)
        if key in FOLDER_GENERIC or re.fullmatch(r'(?:wip|stems?|finals?|masters?)\d*', key):
            continue
        t, _ = main_title(re.sub(r' Project$', '', p))
        if t and not generic(t) and norm(t) not in FOLDER_GENERIC:
            return t
    return ''
