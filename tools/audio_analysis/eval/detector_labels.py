"""Shared event labeller: one Family config turns Peter's projects and WIP mixdowns into held-out labels.

Known times only (Peter, 2026-10-11: no result counts unless the labels are certain). Per song, in mix time (s), the
labels file holds (detector_eval scores against it):
- positives: project MIDI notes on the family's playing tracks or drum-rack pads (excluded voices left out, muted
  pads only on trigger tracks), and the starts of the family's playing audio clips where a loud hit begins within
  clip_onset_s (a placed one-shot), shifted by the song's proven export offset. Muted drum sources are unknown, not
  silent, except groups muted only for a stem export (project_parts, export_groups). Layers within layer_s are one
  event. Only PROMINENT events count (Peter, 2026-10-10: the ones you hear
  in the mix): an attack in the family band (rise >= prominent_db) whose peak is within main_db of the song's main
  events; others are unscored.
- unscored: windows that cannot be judged (quiet events; notes with probability under 1; drum notes on pads or tracks
  no voice names; fast runs under dense_s apart, padded).
- loop_spans: the family's own audio clips (hits past a clip's start are not known times), audio clips that may
  hold the family's events baked in (drum or unclassed parts, part_classes), and muted drum clips; inside them only
  positives count. A song with an every-drum stem (Family.drum_stems) replaces them with the stem's unexplained loud
  moments (drum_stem_blind), and its unknown notes stay unscored only there.
- hard_neg: vouched non-events: kicks, melodic note starts and excluded or other voices more than far_s from any
  positive, outside loop spans (a melodic note in a drum loop may sit on a baked-in snare). For the kick family
  (own_kicks) the kicks are the family's notes: negatives are the rest, far_s from every kick note.
- positives_only: recall-only songs.
Projects are read only through als_extract (read-only, sha-checked).
"""
import glob
import re
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from scipy.signal import butter, sosfilt

from tools.audio_analysis.eval.detector_eval import inside


@dataclass(frozen=True)
class Family:
    name: str
    voice: re.Pattern            # the family's tracks, pads and clip tracks
    exclude: re.Pattern          # look-alike voices that are vouched negatives (rimshots for snares)
    other_voice: re.Pattern      # other voices whose notes are vouched negatives
    other_parts: re.Pattern      # audio clips that may hold the family's events baked in (loop spans)
    not_other_parts: re.Pattern  # ... unless the clip is clearly something else
    band: tuple                  # the family's attack band (Hz) in the mix
    midi_songs: tuple = ()
    clip_songs: tuple = ()
    recall_only: tuple = ()
    wip: bool = True             # WIP songs take their proven offsets from detector_wip
    add_wip: bool = True         # label every proven WIP song (detector_wip), not only the listed ones
    prominent_db: float = 9.0
    main_db: float = 6.0
    dense_s: float = .200
    dense_pad: float = .250
    layer_s: float = .030
    far_s: float = .070
    half: tuple = (-.07, .07)
    stem_band: tuple = (150, 10000)
    clip_onset_s: float = .030  # a clip start is a known hit when its sound begins within this
    kick_clash_s: float = 0.0   # events this close to a kick are unscored: the mix cannot tell them apart
    drop_kick_rolls: bool = False  # kick rolls (roll_spans on the song's kicks) are unscored and untrained
    drum_stems: tuple = ()      # (song, stem path or paths): the song's every-drum stem (drum_stem_blind)
    # (goal song, unknown-stem regex): goal songs whose mix is the sum of their stems (multitrack_song).
    multitracks: tuple = ()
    # The family is the kick: the project's kicks are its own notes, never vouched negatives, and a negative keeps far_s
    # from every one of them, heard or not.
    own_kicks: bool = False
    # Songs the family leaves out though their set is proven: a muted group the bounce played, which export_groups
    # cannot see in a dense mix.
    skip: tuple = ()


def builder_id(module_file):
    """A labels builder's identity for the labels file: module name and the sha256 of its source."""
    import hashlib
    p = Path(module_file)
    return f'{p.stem}@{hashlib.sha256(p.read_bytes()).hexdigest()[:12]}'


def env_db(x, sr, lo, hi):
    """1 ms grid log energy of one band over the last 5 ms."""
    y = sosfilt(butter(4, (lo, hi), btype='band', fs=sr, output='sos'), x) ** 2
    k = int(.005 * sr)
    c = np.concatenate([[0.0], np.cumsum(y)])
    idx = np.round(np.arange(int(len(x) / sr / 1e-3)) * 1e-3 * sr).astype(int)
    return 10 * np.log10((c[np.clip(idx + 1, 0, len(y))] - c[np.clip(idx + 1 - k, 0, len(y))]) / k + 1e-12)


def on_attack(env, t, db=9.0):
    """Whether each time has an attack: the -10..+30 ms peak sits db over the median of -60..-20 ms."""
    i = np.round(np.asarray(t) / 1e-3).astype(int)
    ok = np.zeros(len(i), bool)
    for n, j in enumerate(i):
        if 60 <= j < len(env) - 30:
            ok[n] = env[j - 10:j + 30].max() - np.median(env[j - 60:j - 20]) >= db
    return ok


def prominent(env, t, fam):
    """An attack (on_attack) whose peak is within main_db of the song's main events (90th percentile of the
    attacking ones)."""
    t = np.asarray(t)
    i = np.round(t / 1e-3).astype(int)
    ok = on_attack(env, t, fam.prominent_db)
    lvl = np.array([env[j - 10:j + 30].max() if 60 <= j < len(env) - 30 else -np.inf for j in i])
    if not ok.any():
        return ok
    return ok & (lvl >= np.percentile(lvl[ok], 90) - fam.main_db)


def timing_shift(env, t, fam):
    """Offset (s) in -30..+30 ms that gives the most events with a clear attack. Project offsets can sit ~20 ms off
    (sample attack delay, device latency). Kept at 0 unless another shift beats it by 10% of the events."""
    t = np.asarray(t)
    if not len(t):
        return 0.0
    counts = {s: int(on_attack(env, t + s / 1000, fam.prominent_db).sum()) for s in range(-30, 31)}
    best = max(counts, key=lambda s: (counts[s], -abs(s)))
    return best / 1000 if counts[best] - counts[0] > .1 * len(t) else 0.0


def merge_layers(t, layer_s=.030):
    t = np.sort(np.asarray(t, float))
    return t[np.concatenate([[True], np.diff(t) > layer_s])] if len(t) else t


def drum_rack(devs):
    return any(b.get('key') is not None or drum_rack(b.get('devices')) for d in devs or [] for b in d.get('branches') or [])


def drum_track(tr, fam):
    """A track that may play the family's events: voice-named, part_classes drums, or a MIDI drum rack."""
    from tools.audio_analysis.eval.part_classes import classify
    return bool(fam.voice.search(tr['name'])) or classify(tr['name'], tr.get('groups') or ()) == 'drums' or \
        (tr['kind'] == 'MidiTrack' and drum_rack(tr.get('devices')))


def export_groups(res, fam, x, sr, off, als_dir):
    """Muted groups whose drums sit in the mix anyway: the set was saved muted for a stem export after the mix was
    rendered (Peter, 2026-10-11: murmur's drum group). A muted group counts when the median share of its drum tracks'
    hits that land on a 9 dB broadband attack in the mix reaches max(50%, random times' share + 40 points)."""
    env = env_db(x, sr, 150, 10000)
    dur = len(x) / sr
    base = on_attack(env, np.random.default_rng(0).uniform(1, dur - 1, 400)).mean()
    points = [(q['beat'], q['bpm']) for q in res['tempo_points']]
    out = set()
    for grp in [tr['name'] for tr in res['tracks'] if tr['kind'] == 'GroupTrack' and not tr.get('speaker_on', True)]:
        rates = []
        for tr in res['tracks']:
            if grp not in (tr.get('groups') or ()) or tr['kind'] == 'GroupTrack' or not tr.get('speaker_on', True) or not drum_track(tr, fam):
                continue
            if tr['kind'] == 'MidiTrack':
                ev = np.array([n['s'] for c in tr['midi_clips'] for n in c['notes']])
            else:
                cs = [c for c in res['audio_clips'] if c.get('track') == tr['name'] and not c['disabled']]
                ev = clip_hits(cs, als_dir, sr, (150, 10000), points)[0] if cs else np.zeros(0)
            ev = np.asarray(ev, float) + off
            ev = ev[(ev > 1) & (ev < dur - 1)]
            if len(ev) >= 20:
                rates.append(on_attack(env, ev).mean())
        if rates and np.median(rates) >= max(.5, base + .4):
            out.add(grp)
    return out


def project_parts(res, fam, unmute=()):
    """(family note times, vouched-negative note times, unknown note times, unknown audio spans, family audio clips by
    track; the excluded voices' clip starts under '_neg'). Only playing sources give positives and vouched negatives;
    groups in unmute play (export_groups).
    Muted drum sources are unknown, not silent (a set may be saved muted after its mix was rendered): notes on muted
    drum tracks and pads are unknown, muted drum audio clips and muted MIDI clips on drum tracks are unknown spans.
    Also unknown: notes with probability under 1 and drum notes on pads no pattern names. Muted unclassed audio (full
    song reference bounces) is ignored."""
    from tools.audio_analysis.eval.note_labels import playing_clips, silent
    from tools.audio_analysis.eval.part_classes import classify
    pos, neg, unknown, spans, clips = [], [], [], [], {}
    quiet = silent(res, unmute)
    for tr in res['tracks']:
        trigger = 'trigger' in tr['name'].lower()
        drum = drum_track(tr, fam)
        if tr['name'] in quiet and not trigger:
            unknown += [n['s'] for c in tr['midi_clips'] for n in c['notes']] if drum else []
            continue
        spans += [(c['start_s'], c['end_s']) for c in tr['midi_clips'] if c['disabled']] if drum else []
        played = [n for c in tr['midi_clips'] for n in c['notes']]
        if not played:
            continue
        unknown += [n['s'] for n in played if n['probability'] < 1]
        notes = [n for n in played if n['probability'] >= 1]
        keys, neg_keys, other_keys = set(), set(), set()

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    nm = b.get('name') or ''
                    if b.get('key') is not None and (b.get('on', True) or trigger):
                        if fam.exclude.search(nm):
                            neg_keys.add(b['key'])
                        elif fam.voice.search(nm):
                            keys.add(b['key'])
                        elif fam.other_voice.search(nm):
                            other_keys.add(b['key'])
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        neg += [n['s'] for n in notes if n['key'] in neg_keys | other_keys]
        rest = [n for n in notes if n['key'] not in neg_keys | other_keys]
        if keys:
            pos += [n['s'] for n in rest if n['key'] in keys]
            rest = [n for n in rest if n['key'] not in keys]
        elif fam.voice.search(tr['name']) and not fam.exclude.search(tr['name']):
            pos += [n['s'] for n in rest]
            rest = []
        elif fam.exclude.search(tr['name']) or (fam.other_voice.search(tr['name']) and not fam.voice.search(tr['name'])):
            neg += [n['s'] for n in rest]
            rest = []
        unknown += [n['s'] for n in rest] if drum else []
    playing = {id(c) for c in playing_clips(res, unmute)}
    for c in res['audio_clips']:
        trn, f = c.get('track') or '', str(c.get('file') or '')
        cls = classify(trn, c.get('groups') or (), f)
        if id(c) not in playing:
            if fam.voice.search(trn + f) or cls == 'drums' or (fam.other_parts.search(trn + f) and not fam.not_other_parts.search(trn + f)):
                spans.append((c['start_s'], c['end_s']))
        elif fam.voice.search(trn) and not fam.other_parts.search(trn + f):
            clips.setdefault(trn, []).append(c)
        elif fam.exclude.search(trn + f):
            clips.setdefault('_neg', []).append(c['start_s'])
        elif fam.not_other_parts.search(trn + f):
            continue
        elif fam.other_parts.search(trn + f) or fam.voice.search(trn + f) or cls in ('drums', '?'):
            spans.append((c['start_s'], c['end_s']))
    return np.array(pos), np.array(neg), np.array(unknown), spans, clips


def stem_hits(x, sr, band=(150, 10000)):
    """Attacks in a solo part (stem or clip render): (loud, quiet) times (s); quiet = 12+ dB under the loud ones."""
    e = env_db(x, sr, *band)
    out, last = [], -1000
    for i in range(60, len(e) - 1):
        if i - last < 60:
            continue
        if e[i] - np.median(e[i - 60:i - 20]) >= 10 and e[i] >= e[i - 1] and e[i] >= e[i + 1] - .5:
            j = i + int(np.argmax(e[i:i + 15]))
            out.append(j)
            last = j
    h = np.array(out, int)
    lvl = e[h] if len(h) else np.zeros(0)
    loud = lvl >= (np.percentile(lvl, 90) - 12 if len(h) else 0)
    return h[loud] * 1e-3, h[~loud] * 1e-3


def read_any(path, sr, load):
    """Mono audio at sr: the goal loader for wav, soundfile (mp3, flac, aiff) when it cannot read the file."""
    try:
        return load(path, sr)
    except ValueError:
        import math
        import soundfile as sf
        from scipy.signal import resample_poly
        x, r = sf.read(path, dtype='float64', always_2d=True)
        x = x.mean(axis=1)
        g = math.gcd(sr, r)
        return x if r == sr else resample_poly(x, sr // g, r // g)


def warped_hits(c, x, sr, band, points):
    """(loud, quiet) hits of one warped clip in arrangement seconds: each pass through the clip (the first from its
    start marker, then loop_start..loop_end while it loops) read from the file through the warp markers and placed on
    the tempo map. points: [(beat, bpm)]."""
    from tools.audio_analysis.eval.als_extract import beat_seconds, warp_beats, warp_seconds
    m = c['markers']
    a, cb0 = c['start_beat'], c['clip_start_beat']
    cb1 = c['loop_end'] if c['looped'] else cb0 + c['end_beat'] - c['start_beat']
    loud, quiet = [], []
    while a < c['end_beat'] - 1e-6 and cb1 > cb0:
        cb1 = min(cb1, cb0 + c['end_beat'] - a)
        f0, f1 = max(0.0, warp_seconds(m, cb0)), warp_seconds(m, cb1)
        seg = x[int(f0 * sr):int(f1 * sr)]
        if len(seg) >= sr // 50:
            hl, hq = stem_hits(np.concatenate([np.zeros(sr // 10), seg]), sr, band)
            for hits, out in ((hl, loud), (hq, quiet)):
                out += [beat_seconds(points, a + warp_beats(m, f0 + h - .1) - cb0) for h in hits if h >= .1]
        a += cb1 - cb0
        if not c['looped']:
            break
        cb0, cb1 = c['loop_start'], c['loop_end']
    return loud, quiet


def clip_hits(rows, als_dir, sr, band=(150, 10000), points=None):
    """Hits inside the family's audio clips, in arrangement seconds: one-shots, frozen tracks and consolidated parts
    alike (each is that track rendered on its own, so stem_hits applies). Files missing at their stored path are looked
    up by name under the project folder (read only). With the tempo map (points: [(beat, bpm)]), warped clips are
    placed through their warp markers and loops (warped_hits); without it, or for unwarped clips, file time runs 1:1
    from the start marker. Returns (loud, quiet)."""
    from tools.audio_analysis.eval.kick_goal_eval import load
    loud, quiet, found, cache = [], [], {}, {}
    for c in rows:
        name = c.get('file')
        if not name:
            continue
        p = c.get('file_path')
        if not (p and Path(p).exists()):
            if name not in found:
                found[name] = next((str(q) for q in Path(als_dir).rglob(glob.escape(name)) if q.is_file()), None)
            p = found[name]
        if not p:
            continue
        if p not in cache:
            cache[p] = read_any(p, sr, load)
        if points is not None and c['warped'] and len(c['markers']) >= 2:
            h_loud, h_quiet = warped_hits(c, cache[p], sr, band, points)
            loud += h_loud
            quiet += h_quiet
            continue
        a = int(max(0.0, c['file_start_s']) * sr)
        seg = cache[p][a:a + int((c['end_s'] - c['start_s']) * sr)]
        if len(seg) < sr // 50:
            continue
        h_loud, h_quiet = stem_hits(np.concatenate([np.zeros(sr // 10), seg]), sr, band)
        loud += list(c['start_s'] + h_loud - .1)
        quiet += list(c['start_s'] + h_quiet - .1)
    return np.array(loud), np.array(quiet)


def clip_starts(clips, als_dir, sr, band, points, within):
    """Starts (s) of the clips whose sound begins there: a loud hit (clip_hits) within -5 ms..within of the start."""
    loud, _ = clip_hits(clips, als_dir, sr, band, points)
    starts = np.array(sorted({c['start_s'] for c in clips}))
    if not len(loud):
        return starts[:0]
    return starts[[bool(np.any((loud >= s - .005) & (loud <= s + within))) for s in starts]]


def dense_spans(hits, fam):
    """Fast runs (breaks, rolls): labelled events under dense_s apart, padded by dense_pad and joined when under 1 s
    apart. Unscored for now (BUG-9ngk8.2.1 (fast breakbeats and rolls))."""
    h = np.sort(np.asarray(hits, float))
    spans = []
    for a, b in zip(h[:-1], h[1:]):
        if b - a < fam.dense_s:
            lo, hi = a - fam.dense_pad, b + fam.dense_pad
            if spans and lo - spans[-1][1] < 1.0:
                spans[-1][1] = max(spans[-1][1], hi)
            else:
                spans.append([lo, hi])
    return spans


def roll_spans(hits, gap_s=.200, min_hits=4, pad=.250):
    """Rolls: runs of at least min_hits hits, each under gap_s after the last, padded by pad. Unscored and untrained
    (Peter, 2026-10-11: "remove the rolls ... get the basics and foundations correct first"); a double or triple hit
    stays a hit."""
    h = np.sort(np.asarray(hits, float))
    spans, start = [], 0
    for i in range(1, len(h) + 1):
        if i == len(h) or h[i] - h[i - 1] >= gap_s:
            if i - start >= min_hits:
                spans.append([h[start] - pad, h[i - 1] + pad])
            start = i
    return spans


def stem_lag(stem, x, sr, band, search_s=20.0):
    """(L, z): a stem event at t sits at t + L in the mix, from the cross-correlation of the two signals' attacks
    (rises of env_db in band) within +-search_s; z is the peak's height over the other lags."""
    def attacks(y):
        e = env_db(y, sr, *band)
        return np.maximum(np.diff(e, prepend=e[0]), 0.0)
    s, m = attacks(stem), attacks(x)
    n = 1 << int(np.ceil(np.log2(len(s) + len(m))))
    c = np.fft.irfft(np.fft.rfft(m, n) * np.conj(np.fft.rfft(s, n)), n)
    k = int(search_s * 1e3)
    lags = np.r_[np.arange(0, k), np.arange(-k, 0)]
    vals = np.r_[c[:k], c[-k:]]
    i = int(np.argmax(vals))
    return lags[i] * 1e-3, float((vals[i] - vals.mean()) / (vals.std() + 1e-12))


def drum_stem_blind(fam, t, x, sr, pos, explained, proven_lag=None):
    """Spans (mix s) where the song's drum stem may hold an unlabelled family event, or None without a stem.

    The drum stem is every drum and nothing else (Peter, 2026-10-11): where its family band stays more than
    prominent_db under the labelled events' level, no prominent family event plays, so the moment is known. Loud
    frames not within -10..+60 ms of an explained time (positives, dropped events, kicks) stay unknown, padded by
    the half window. A song with no labelled events (its snares play inside breaks and loops) takes the median of the
    stem's own loud attacks as that level, so only moments with no loud drum at all become known. Exits when the stem
    does not line up with the mix: z under LAG_Z_MIN, unless its lag is the song's proven stem lag (the kick stem from
    the same export) within 5 ms."""
    import sys

    from tools.audio_analysis.eval.kick_goal_labels import LAG_Z_MIN, load
    paths = dict(fam.drum_stems).get(t)
    if not paths:
        return None
    stem = load(paths, sr)
    L, z = stem_lag(stem, x, sr, fam.band)
    if z < LAG_Z_MIN and not (proven_lag is not None and abs(L - proven_lag) <= .005):
        sys.exit(f'{t}: drum stem does not line up with the mix (lag {L:.3f} s, z {z:.1f})')
    e = env_db(stem, sr, *fam.band)
    ms = lambda s: np.clip(np.round((np.asarray(s, float) - L) * 1e3).astype(int), 0, len(e) - 1)  # noqa: E731
    if len(pos):
        main = np.median([e[ms(p - .01):ms(p + .03) + 1].max() for p in pos])
    else:
        loud_hits = stem_hits(stem, sr, fam.band)[0]
        if not len(loud_hits):
            return None
        main = np.median(e[np.clip(np.round(loud_hits * 1e3).astype(int), 0, len(e) - 1)])
    loud = e >= main - fam.prominent_db
    for u in ms(np.asarray(explained, float)):
        loud[max(0, u - 10):u + 60] = False
    i = np.flatnonzero(loud)
    if not len(i):
        return []
    cuts = np.flatnonzero(np.diff(i) > 30)
    starts, ends = i[np.r_[0, cuts + 1]], i[np.r_[cuts, len(i) - 1]]
    h0, h1 = fam.half
    return [(s * 1e-3 + L + h0, e_ * 1e-3 + L + h1) for s, e_ in zip(starts, ends)]


def cover(spans):
    tot, end = 0.0, -1e9
    for a, b in sorted(spans):
        a = max(a, end)
        tot += max(0.0, b - a)
        end = max(end, b)
    return tot


def song(g, t, fam):
    """One song's labels dict (see the module doc)."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    from tools.audio_analysis.eval.detector_songs import audio, rate, wip
    W = wip() if fam.wip else {}
    sr = rate(g, t)
    x = audio(g, t)
    dur = len(x) / sr
    env = env_db(x, sr, *fam.band)
    h0, h1 = fam.half
    unscored = []
    src = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
    res = extract(src[0])
    off = src[1]
    unmute = export_groups(res, fam, x, sr, off, Path(src[0]).parent)
    notes, neg_notes, unknown, spans, clips = project_parts(res, fam, unmute)
    own = [c for k, v in clips.items() if k != '_neg' for c in v]
    loops = [(a + off, b + off) for a, b in spans + [(c['start_s'], c['end_s']) for c in own]]
    neg_clips = list(clips.get('_neg', []))
    neg_own = np.concatenate([neg_notes, neg_clips]) + off if len(neg_notes) or neg_clips else np.zeros(0)
    unknown_w = [(u + off + h0, u + off + h1) for u in unknown]
    points = [(q['beat'], q['bpm']) for q in res['tempo_points']]
    placed = clip_starts(own, Path(src[0]).parent, sr, fam.stem_band, points, fam.clip_onset_s)
    pos = merge_layers(np.concatenate([notes, placed]) + off, fam.layer_s)
    pos = pos[(pos > .1) & (pos < dur - .1)]
    shift = timing_shift(env, pos, fam)
    pos = pos + shift
    ok = prominent(env, pos, fam)
    dropped = list(pos[~ok])
    unscored += [(u + h0, u + h1) for u in pos[~ok]]
    pos = pos[ok]
    kicks, mel = vouched(g, t, W, src, res if t in W else None)
    if fam.kick_clash_s and len(pos) and len(kicks):
        from tools.audio_analysis.eval.detector_eval import nearest
        clash = nearest(pos, kicks) <= fam.kick_clash_s
        unscored += [(u + h0, u + h1) for u in pos[clash]]
        pos = pos[~clash]
    blind = drum_stem_blind(fam, t, x, sr, pos, np.concatenate([pos, dropped, kicks]),
                           g.records.get(t, {}).get('lag'))
    if blind is not None:
        # The drum stem shows every drum: only its unexplained loud moments stay unknown.
        loops = blind
        unknown_w = [w for w in unknown_w if inside(np.array([(w[0] + w[1]) / 2]), blind)[0]] if blind else []
    unscored += unknown_w
    dense = dense_spans(np.concatenate([pos, dropped]), fam)
    if fam.drop_kick_rolls and len(kicks):
        dense = sorted(dense + roll_spans(kicks))
    # Events inside fast runs are not scored, but they are real: kept for training on runs (BUG-9ngk8.2.1).
    every = np.sort(np.concatenate([pos, dropped]))
    dense_all = every[inside(every, dense) & (every > .1) & (every < dur - .1)]
    dense_pos = pos[inside(pos, dense) & (pos > .1) & (pos < dur - .1)]
    pos = pos[~inside(pos, dense)]
    unscored += dense
    pos = pos[(pos > .1) & (pos < dur - .1)]
    # Proven kicks are not the family's sound even inside unknown audio (Peter, 2026-10-11); other negatives inside
    # unknown audio stay unknown.
    neg = np.concatenate([mel, neg_own])
    if fam.own_kicks:
        neg = neg[~inside(neg, loops)] if loops else neg
        ref = np.sort(np.concatenate([every, kicks]))
    else:
        neg = np.concatenate([kicks, neg[~inside(neg, loops)] if loops else neg])
        ref = pos
    if len(ref):
        j = np.clip(np.searchsorted(ref, neg), 1, len(ref) - 1) if len(ref) > 1 else np.zeros(len(neg), int)
        near = np.minimum(np.abs(neg - ref[np.clip(j - 1, 0, len(ref) - 1)]), np.abs(neg - ref[j]))
        neg = neg[near > fam.far_s]
    return dict(kind='project', duration_s=dur, positives=sorted(map(float, pos)), unscored=[list(map(float, u)) for u in unscored],
                loop_spans=[list(map(float, u)) for u in loops], hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))),
                positives_only=t in fam.recall_only, dropped=sorted(map(float, dropped)), dense=dense,
                dense_pos=sorted(map(float, dense_pos)), dense_all=sorted(map(float, dense_all)),
                timing_shift_ms=round(1000 * shift, 1), offset_s=float(off), unmuted=sorted(unmute), drum_stem=blind is not None,
                sources=dict(midi=len(notes), clip_start=len(placed)), builder=builder_id(__file__))


def multitrack_song(g, t, fam, unknown):
    """One labels dict for a goal song whose mix is the sum of its stems (kick_goal_labels.stem_sum): every part is
    known by its stem. Positives are the attacks of the family's stems (voice-named, not excluded), kept when
    prominent in the mix as for project songs. Attacks of other-voice stems (hats, rides) and the song's proven kicks
    are vouched negatives. Stems matching unknown may hold the family's sound (snaps, beatbox, fills, percussion, drum
    loops): where one sits within prominent_db of the positives' stem level (without positives, of its own loud
    attacks), the moment is unknown. Every other moment is known, so whatever sounds there is not the family's."""
    from tools.audio_analysis.eval.detector_songs import audio, rate
    from tools.audio_analysis.eval.kick_goal_labels import load
    sr = rate(g, t)
    x = audio(g, t)
    stems = [Path(q) for q in g.labels['new'][t]['mix_parts']]
    xs = [load(str(q), sr) for q in stems]
    total = np.zeros(len(x))
    for y in xs:
        total[:len(y)] += y[:len(x)]
    if np.max(np.abs(total - x)) > 1e-4:
        raise SystemExit(f'{t}: the goal mix is not the sum of its stems')
    dur = len(x) / sr
    env = env_db(x, sr, *fam.band)
    h0, h1 = fam.half
    role = {}
    for q in stems:
        n = q.stem
        role[q] = ('unknown' if unknown.search(n) else 'voice' if fam.voice.search(n) and not fam.exclude.search(n)
                   else 'other' if fam.other_voice.search(n) else 'music')
    hits = {q: np.concatenate(stem_hits(y, sr, fam.band)) for q, y in zip(stems, xs) if role[q] != 'music'}
    voice = [i for i, q in enumerate(stems) if role[q] == 'voice']
    pos = merge_layers(np.concatenate([hits[stems[i]] for i in voice]) if voice else np.zeros(0), fam.layer_s)
    pos = pos[(pos > .1) & (pos < dur - .1)]
    shift = timing_shift(env, pos, fam)
    pos = pos + shift
    ok = prominent(env, pos, fam)
    dropped = pos[~ok]
    unscored = [(u + h0, u + h1) for u in dropped]
    pos = pos[ok]
    main = None
    if len(pos):
        vsum = np.zeros(max(len(xs[i]) for i in voice))
        for i in voice:
            vsum[:len(xs[i])] += xs[i]
        ve = env_db(vsum, sr, *fam.band)
        at = np.round((pos - shift) * 1e3).astype(int)
        main = np.median([ve[j - 10:j + 30].max() for j in at])
    elif any(r == 'unknown' for r in role.values()):
        lv = [env_db(y, sr, *fam.band)[np.round(stem_hits(y, sr, fam.band)[0] * 1e3).astype(int)]
              for q, y in zip(stems, xs) if role[q] == 'unknown']
        main = np.median(np.concatenate(lv)) if sum(map(len, lv)) else None
    loops = []
    for q, y in zip(stems, xs):
        if role[q] == 'unknown' and main is not None:
            i = np.flatnonzero(env_db(y, sr, *fam.band) >= main - fam.prominent_db)
            if len(i):
                cuts = np.flatnonzero(np.diff(i) > 30)
                loops += [(a * 1e-3 + h0, b * 1e-3 + h1) for a, b in zip(i[np.r_[0, cuts + 1]], i[np.r_[cuts, len(i) - 1]])]
    kicks, _ = vouched(g, t, {}, None, None)
    dense = dense_spans(np.concatenate([pos, dropped]), fam)
    if fam.drop_kick_rolls and len(kicks):
        dense = sorted(dense + roll_spans(kicks))
    every = np.sort(np.concatenate([pos, dropped]))
    dense_all = every[inside(every, dense)]
    dense_pos = pos[inside(pos, dense)]
    pos = pos[~inside(pos, dense)]
    unscored += dense
    neg = np.sort(np.concatenate([hits[q] for q in stems if role[q] == 'other'] + [np.zeros(0)]))
    neg = np.concatenate([kicks, neg[~inside(neg, loops)] if loops else neg])
    if len(pos) and len(neg):
        neg = neg[np.min(np.abs(neg[:, None] - pos[None, :]), axis=1) > fam.far_s]
    return dict(kind='project', duration_s=dur, positives=sorted(map(float, pos)), unscored=[list(map(float, u)) for u in unscored],
                loop_spans=[list(map(float, u)) for u in loops], hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))),
                positives_only=t in fam.recall_only, dropped=sorted(map(float, dropped)), dense=dense,
                dense_pos=sorted(map(float, dense_pos)), dense_all=sorted(map(float, dense_all)),
                timing_shift_ms=round(1000 * shift, 1), offset_s=0.0, unmuted=[], drum_stem=True, proof='stem',
                sources=dict(stems={q.name: role[q] for q in stems}), builder=builder_id(__file__))


def vouched(g, t, W, src, res):
    """(kick times, melodic note starts) in mix time: WIP songs straight from the project, shifted by the proven
    offset; goal songs from the kick labels and kick_goal_melodic."""
    from tools.audio_analysis.eval.kick_goal_melodic import melodic_onsets
    if t in W:
        from tools.audio_analysis.eval.detector_wip import kick_beats
        from tools.audio_analysis.eval.kick_goal_melodic import NOT_MELODIC
        kicks = kick_beats(res) * 60 / W[t]['bpm'] + src[1]
        mel = np.array([n['s'] for tr in res['tracks'] if tr.get('speaker_on', True) and not NOT_MELODIC.search(tr['name'])
                        for c in tr['midi_clips'] for n in c['notes']]) + src[1]
        return kicks, mel
    r = g.records.get(t, {})
    kicks = np.asarray(r.get('stem_kicks') if r.get('stem_kicks') is not None else r.get('all_labels', r.get('truth', [])), float)
    return kicks, melodic_onsets(t) + r.get('lag', 0.0)


def songs(fam):
    """Every song the family labels, in file order."""
    from tools.audio_analysis.eval.detector_songs import wip
    listed = fam.midi_songs + fam.clip_songs + (tuple(wip()) if fam.wip and fam.add_wip else ()) + \
        tuple(m[0] for m in fam.multitracks)
    return tuple(t for t in dict.fromkeys(listed) if t not in fam.skip)


def write(fam, path, log=print):
    """Label every song of the family and write the labels file (json)."""
    import json
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    g = Goal(mode='v3')
    out = {}
    multi = dict(fam.multitracks)
    for t in songs(fam):
        out[t] = s = multitrack_song(g, t, fam, multi[t]) if t in multi else song(g, t, fam)
        log(f"{t:14s} {s['kind']:5s} {len(s['positives']):4d} events, {len(s['unscored']):4d} unscored windows "
            f"(loops cover {cover(s['loop_spans']):5.1f} of {s['duration_s']:.0f} s), {len(s['hard_neg']):5d} vouched negatives"
            f"{' (recall only)' if s['positives_only'] else ''}{' playing: ' + ', '.join(s['unmuted']) if s['unmuted'] else ''}")
    Path(path).write_text(json.dumps(out))
    log(f"total {sum(len(s['positives']) for s in out.values())} {fam.name} events in {len(out)} songs -> {path}")
    return out
