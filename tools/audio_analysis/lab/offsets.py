#!/usr/bin/env python3
"""Export-offset proofs for project songs (docs/DETECTOR_LAB_DESIGN.md D5), by detector_wip's method on the song's
own mix: the project's kick notes against the mix's low onset strength, its snare notes against the 1-8 kHz
rises, each fitted to whole bars plus a lag, the offset chosen by detector_wip.export_offset. Proven when both
patterns pick the same bar, or when only one pattern exists and it wins clearly (r >= 0.3, margin >= 1 sd). A proof
counts only when its offset equals the one the labels use (within 1 ms). A WIP song (detector_wip) is checked against its recorded set and offset, on the audio the lab
reads. Writes lab/offset_proofs.json; registry.py reads it.

--variants proves the mix and master variants the catalogue judged same_arrangement against a song's lab mix
(catalog.py variants): each by the same fit on the variant's own audio, against the offset its labels would use (the
song's offset less the catalogue's lag); failing that, relatively: one pattern fitted on both the song's mix and the
variant moves by the catalogue's lag within VARIANT_TOL_S, so the song's proven offset carries over. Variants train and
are never scored, and the catalogue's whole-file lag is the alignment; the pattern only confirms it, and mastering
moves a fitted kick attack by up to 10 ms. Any audio the catalogue reads (mp3, aiff, flac) is proven. Identical audio and
byte-identical copies of the lab mix or of another variant are listed, not proven. Writes lab/variant_proofs.json, which
sampling.mix_variants reads (registry.variants_of).

Usage: offsets.py SONG...
       offsets.py --variants [SONG...]"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

PROOFS = Path(__file__).with_name('offset_proofs.json')
VARIANT_TOL_S = .010   # how far mastering legitimately moves a fitted kick attack; variants only, never the exam's 1 ms


def fits(song, als):
    """(kick fit, snare fit, seconds per beat) of the song's mix against its set's notes, or None under tempo
    automation. A fit is None when the set has no such notes."""
    from scipy.ndimage import maximum_filter1d
    from scipy.signal import resample_poly
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_wip import kick_beats, prove as fit, snare_beats
    from tools.audio_analysis.eval.kick_goal_wip_sections import band_amp_rise, onset_env
    from tools.audio_analysis.lab.registry import mix
    res = extract(als)
    if len(res['tempo_points']) != 1:
        return None
    x = mix(song)
    if song.sample_rate != 48000:
        x = resample_poly(x, 48000, song.sample_rate)
    spb, dur = 60 / res['tempo_points'][0]['bpm'], len(x) / 48000
    kick = fit(onset_env(x), kick_beats(res), dur, spb)
    snare = fit(100 * maximum_filter1d(band_amp_rise(x, 1000, 8000), 7), snare_beats(res), dur, spb)
    return kick, snare, spb


def prove(song, als, used, got=False):
    """got: fits(song, als) when already computed."""
    from tools.audio_analysis.eval.detector_wip import export_offset
    got = fits(song, als) if got is False else got
    if got is None:
        return dict(status='not proven (tempo automation)')
    kick, snare, spb = got
    chosen = export_offset(kick, snare, spb)
    if chosen is None:
        return dict(status='not proven', kick=kick, snare=snare)
    status, off = chosen
    if abs(off - used) > .001:
        return dict(status=f'not proven (fit {off:.4f} s, labels use {used:.4f} s)', kick=kick, snare=snare)
    return dict(status=status, offset_s=round(off, 4), kick=kick, snare=snare)


def catalogue_variants(con, song_id):
    """[(path, sha256, lag_s, identical)] of the song's variants the catalogue judged same_arrangement against its lab
    mix; aligned_needs_check and different_arrangement never transfer labels, however close."""
    import json as _json
    rows = con.execute("SELECT v.path, a.sha256, v.lag_s, v.detail FROM variants v LEFT JOIN assets a USING(path) "
                       "WHERE v.song_id=? AND v.ref='lab_mix' AND v.verdict='same_arrangement' ORDER BY v.path",
                       (song_id,)).fetchall()
    return [(p, sha, lag, bool(_json.loads(d or '{}').get('identical'))) for p, sha, lag, d in rows]


def moves(parent_fits, variant_fits, lag_s):
    """{pattern: seconds its fit misses moving by the catalogue's lag} for every pattern fitted on both the song's mix
    and the variant. The fit's bias on a mix is the song's, so it cancels."""
    if parent_fits is None or variant_fits is None:
        return {}
    from tools.audio_analysis.eval.detector_wip import implied_offsets
    a, b = implied_offsets(*parent_fits), implied_offsets(*variant_fits)
    return {k: (b[k] - a[k]) + lag_s for k in sorted(set(a) & set(b))}


def relative(moved):
    """The relative proof from moves(): (pattern, residual s) of the pattern nearest the lag when it is within
    VARIANT_TOL_S, else None. The song's own offset is proven apart (its audit)."""
    if not moved:
        return None
    k = min(moved, key=lambda p: (abs(moved[p]), p))
    return (k, moved[k]) if abs(moved[k]) <= VARIANT_TOL_S else None


def prove_variant(song, path, sha, lag_s, als, seen, parent_fits=None):
    """One proof row for a variant of song. seen: {decoded audio sha256: variant id} of the song's audio so far;
    parent_fits: fits(song, als). Proven absolutely (prove: the fit lands on the offset its labels use) or relatively
    (relative: the fit moves with the catalogue's lag)."""
    import hashlib
    from tools.audio_analysis.lab.registry import mix, sha256_file, variant_copy, variant_id
    vid = variant_id(song.id, sha)
    row = dict(parent=song.id, path=path, sha256=sha, lag_s=lag_s, als=str(als) if als else None)
    if sha in song.source_sha256:
        return vid, dict(row, status='excluded (a byte-identical copy of the lab mix)')
    if song.offset_s is None or als is None:
        return vid, dict(row, status='not proven (the song has no export offset or Ableton set)')
    from tools.audio_analysis.catalog.catalog import is_dataless
    if is_dataless(path):
        return vid, dict(row, status='not proven (online-only file: never downloaded by a proof)')
    if sha256_file(path) != sha:
        return vid, dict(row, status='not proven (the file changed since the catalogue hashed it)')
    used = song.offset_s - lag_s
    v = variant_copy(song, dict(path=path, sha256=sha, offset_s=used, status='pending'))
    try:
        got = fits(v, als)
        res = prove(v, als, used, got)
        moved = moves(parent_fits, got, lag_s)
        rel = None if res['status'].startswith('proven') else relative(moved)
        if rel:
            res = dict(res, status=f'proven (relative: the {rel[0]} fit moves with the catalogue lag, '
                                   f'{1e3 * rel[1]:+.2f} ms)')
        x = mix(v)
    except Exception as e:  # an unreadable variant is listed, never fatal
        return vid, dict(row, status=f'not proven (unreadable: {type(e).__name__}: {e})'[:200])
    audio = hashlib.sha256(np.ascontiguousarray(x).tobytes()).hexdigest()
    row = dict(row, audio_sha256=audio, duration_s=round(len(x) / v.sample_rate, 4), fit=res.get('offset_s'),
               moves_ms={k: round(1e3 * r, 3) for k, r in moved.items()}, status=res['status'])
    if audio in seen:
        return vid, dict(row, status=f'excluded (same audio as {seen[audio]})')
    seen[audio] = vid
    if rel:
        row['relative'] = dict(pattern=rel[0], residual_ms=round(1e3 * rel[1], 3))
    return vid, dict(row, offset_s=used) if res['status'].startswith('proven') else row


def variants(ids):
    """Proves every same_arrangement variant of the given project songs (all when none) into VARIANT_PROOFS."""
    import hashlib
    from tools.audio_analysis.catalog.catalog import connect
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    from tools.audio_analysis.lab.registry import VARIANT_PROOFS, load, mix, variant_id
    songs = load()
    W = wip()
    con = connect(readonly=True)
    proofs = json.loads(VARIANT_PROOFS.read_text()) if VARIANT_PROOFS.exists() else {}
    for t, s in songs.items():
        if s.kind != 'project' or (ids and t not in ids):
            continue
        found = catalogue_variants(con, t)
        proofs = {k: v for k, v in proofs.items() if v['parent'] != t}
        if not found:
            continue
        als = W[t]['als'] if t in W else (_source(t) or (None,))[0]
        seen = {hashlib.sha256(np.ascontiguousarray(mix(s)).tobytes()).hexdigest(): f'{t} (the lab mix)'}
        parent_fits = fits(s, als) if als else None
        for path, sha, lag, identical in found:
            if variant_id(t, sha) in proofs:
                continue   # a byte-identical copy of a file already judged: one variant, its first path
            if identical:
                proofs[variant_id(t, sha)] = dict(parent=t, path=path, sha256=sha, lag_s=lag,
                                                  status='excluded (identical audio to the lab mix)')
                continue
            vid, row = prove_variant(s, path, sha, lag, als, seen, parent_fits)
            proofs[vid] = row
            print(f"{vid:28s} {row['status']}", flush=True)
        n = sum(1 for v in proofs.values() if v['parent'] == t and v['status'].startswith('proven'))
        print(f'{t}: {n} of {len(found)} same_arrangement variants proven', flush=True)
    VARIANT_PROOFS.write_text(json.dumps(proofs, indent=1, sort_keys=True) + '\n')


def source(t):
    """(set, export offset the labels use) for a goal song with a project or a WIP song."""
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    w = wip().get(t)
    return _source(t) or (w and (w['als'], w['offset_s']))


def main():
    from tools.audio_analysis.lab.registry import load
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if sys.argv[1] == '--variants':
        return variants(set(sys.argv[2:]))
    songs = load()
    proofs = json.loads(PROOFS.read_text()) if PROOFS.exists() else {}
    for t in sys.argv[1:]:
        als, used = source(t)
        proofs[t] = dict(prove(songs[t], als, used), als=str(als))
        print(f"{t:18s} {proofs[t]['status']}")
    PROOFS.write_text(json.dumps(proofs, indent=1, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
