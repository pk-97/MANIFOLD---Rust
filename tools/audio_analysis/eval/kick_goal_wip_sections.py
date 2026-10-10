#!/usr/bin/env python3
"""Kick labels for usable 8-bar sections of a WIP full mix.

Usage: kick_goal_wip_sections.py PROJECT [PROJECT ...]   (writes ~/.cache/manifold/ableton/wip_labels/PROJECT.json)

Timing proof: export offset = -(whole bars) + one constant lag (+-80 ms). Each candidate is scored over the
whole song by the z-score of the 40-150 Hz onset envelope sampled at every kick note (a smoothed
impulse-train correlation). Audio-sample kicks are rebuilt from their clips and cross-correlated with the mix.
Sections: 8 bars on the arrangement grid. Unusable when a kick-carrying drum source plays there.
"""
import json
import math
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402
from scipy.ndimage import maximum_filter1d  # noqa: E402
from scipy.signal import butter, correlate, resample_poly, sosfilt, sosfiltfilt  # noqa: E402

from tools.audio_analysis.eval.kick_goal_trigger_labels import nearest, strong_low_hits  # noqa: E402

SR = 48000
R = '/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects/'
C = Path.home() / '.cache/manifold/ableton'
OUT = C / 'wip_labels'
LAGS = np.arange(-80, 81) / 1000
SEC_BEATS = 32
HIT = .02

P = R + '2026/'
PROJECTS = {
    'ctrl_corrosion': (P + 'Katsu Don Project/FINALS/Corrosion - FINAL 01.wav', 'Katsu Don - V4', ['44-DS Kick']),
    'got_it': (P + 'MudPie Project/Got It Wip 5.mp3', 'Got It Wip 5', ['32-DS Kick', '40-DS Kick']),
    '48_hours': (P + '48 Hours Project/48 Hours - WIP 2.mp3', '48 Hours V3', ['49-DS Kick']),
    'default_haze': (P + 'Default Haze Project/Default Haze - WIP 2.mp3', 'Default Haze', ['45-DS Kick']),
    'dusk2037': (P + 'Dusk2026 Project/Dusk2037 - WIP 1.mp3', 'Dusk2037', ['45-DS Kick']),
    'facade': (P + 'Facade Project/Facade - WIP 4.mp3', 'Facade', ['37-DS Kick']),
    'flight': (P + 'Flight - 2026 Edits Project/Flight - WIP 7 - 2026mix.mp3', 'Flight - WIP 7 -  2026 Mix', ['51 Kick']),
    'lowkey': (P + "I Guess It's Just This Project/Lowkey - WIP 9.mp3", 'Lowkey V2 - Mix WIP 1', ['27-DS Kick']),
    'lift_me': (P + 'Lift Me Project/Lift Me x LATENT SPACE - WIP 2.mp3', 'Lift Me [12.5.26] x LATENT SPACE ',
                ['audio:4-05 JR Kick', 'audio:5-05 JR Kick', 'audio:6-05 JR Kick']),
    'oppression': (P + 'Oprression/Opression - WIP 1.mp3', 'Opression - V2', ['37-DS Kick']),
    'overwhelming_force': (P + 'Overwhelming Force Project/Overwhelming Force - WIP 3.mp3', 'Overwhelming Force v2', ['36-DS Kick']),
    'recognition': (P + 'Recognition Project/Recognition (Drown With Me) - WIP 4.mp3', 'Recognition V2 - Mix WIP 1', ['69-DS Kick']),
    'renew': (P + 'Renew Project/Renew - WIP 1.mp3', 'Renew', ['31-DS Kick']),
}

INCL = re.compile(r'drum|break|kit|groove|beat|kick|bd_|linn|909', re.I)
EXCL = re.compile(r'top|hat|shaker|ride|cymbal|snare|clap|rim|tom|vocal|vox|synth|atmos|riser|melody|piano|chord|pad|fx', re.I)
KICK_PAD = re.compile(r'kick|\bbd\b|bass ?drum', re.I)


def load(path):
    x, sr = sf.read(path, dtype='float64', always_2d=True)
    x = x.mean(axis=1)
    if sr != SR:
        g = math.gcd(sr, SR)
        x = resample_poly(x, SR // g, sr // g)
    return x


def low_env(x):
    lo = sosfilt(butter(4, [40, 150], btype='band', fs=SR, output='sos'), x)
    hop = SR // 1000
    n = len(lo) // hop
    e = 10 * np.log10(np.mean(lo[:n * hop].reshape(n, hop) ** 2, axis=1) + 1e-12)
    rise = np.zeros(n)
    rise[10:] = e[10:] - e[:-10]
    return e, rise


def muted_map(res):
    groups = {t['name']: t['speaker_on'] for t in res['tracks'] if t['kind'] == 'GroupTrack'}
    return {t['name']: (not t['speaker_on']) or any(groups.get(g) is False for g in t['groups']) for t in res['tracks']}


def kick_notes(res, sources):
    """Arrangement seconds and beats of every kick onset, plus clip list for audio sources."""
    s, b, clips = [], [], []
    for src in sources:
        if src.startswith('audio:'):
            name = src[6:]
            for c in res['audio_clips']:
                if c['track'] == name and not c['disabled'] and not c['file'].endswith(' R.wav') and c['file_start_s'] < .01:
                    s.append(c['start_s'])
                    b.append(c['start_beat'])
                    clips.append(c)
        else:
            t = next(t for t in res['tracks'] if t['name'] == src)
            for c in t['midi_clips']:
                for n in c['notes']:
                    s.append(n['s'])
                    b.append(n['beat'])
    s, idx = np.unique(np.round(s, 4), return_index=True)
    return s, np.array(b)[idx], clips


def other_drums(res, sources):
    """[(start_beat, end_beat, label)] for kick-carrying drum sources other than the kick itself."""
    src_names = {x.split(':', 1)[-1] for x in sources}
    muted = muted_map(res)
    spans = []
    for c in res['audio_clips']:
        if c['disabled'] or muted.get(c['track']) or c['track'] in src_names:
            continue
        clip_txt = f"{c['name']} {c['file']}"
        if (INCL.search(clip_txt) and not EXCL.search(clip_txt)) or re.search(r'kick|linn', c['track'], re.I):
            spans.append((c['start_beat'], c['end_beat'], f"audio {c['track']}: {c['name']}", c['file_path']))
    for t in res['tracks']:
        if t['kind'] != 'MidiTrack' or t['name'] in src_names or muted.get(t['name']):
            continue
        pads = {}

        def walk(devs):
            for d in devs:
                if d['kind'] == 'DrumGroupDevice':
                    for br in d.get('branches') or []:
                        if br.get('key') is not None:
                            pads[br['key']] = br['name'] or ''
                for br in d.get('branches') or []:
                    walk(br.get('devices') or [])
        walk(t['devices'])
        plug = ' '.join((d.get('plugin') or d['name']) for d in t['devices'])
        for c in t['midi_clips']:
            for n in c['notes']:
                pad = pads.get(n['key'])
                kicky = (pad is not None and (KICK_PAD.search(pad) or (pad == '' and n['key'] in (35, 36)))) or \
                    (not pads and KICK_PAD.search(plug + ' ' + t['name']))
                if kicky:
                    spans.append((n['beat'], n['beat'] + max(n['duration'], .25), f"midi {t['name']}: {pad or plug}", None))
    return spans




LOOP_KICK_RATE = .3  # kick-shaped hits per second at which a drum loop counts as kick-carrying


def loop_kick_rate(path):
    """Kick-shaped hits per second in a drum-loop file: strong 40-150 Hz hits with at least half their first 40 ms
    below 140 Hz (kick_goal_labels.low_share). 1.0 when the file cannot be read."""
    from tools.audio_analysis.eval.kick_goal_labels import low_share
    try:
        y = load(path)
    except Exception:
        return 1.0
    if len(y) < SR // 2:
        return 1.0
    h = strong_low_hits(y, SR)
    return round(sum(low_share(y, SR, t) >= .5 for t in h) / (len(y) / SR), 2)
UNMARKED_MAX = 2  # kick-like onsets off every kick note allowed in a usable 8-bar section
ENV_BIAS = .0066  # onset-envelope peak sits this long after the physical attack (Lift Me rebuild vs grid)
def rebuild_xcorr(x, clips, dur):
    """Rebuild the kick track from its one-shot clips and cross-correlate with the mix (low band, 2 kHz)."""
    cache = {}
    end = max(c['end_s'] for c in clips) + 1
    y = np.zeros(int(end * SR))
    for c in clips:
        if c['file_path'] not in cache:
            cache[c['file_path']] = load(c['file_path'])
        smp = cache[c['file_path']]
        a = int(c['loop_start'] * SR)
        n = min(int((c['end_s'] - c['start_s']) * SR), len(smp) - a)
        i = int(c['start_s'] * SR)
        y[i:i + n] += smp[a:a + n]
    # GCC-PHAT on 30-400 Hz at 2 kHz: whitening keeps only phase agreement, which sharpens the true peak
    sos = butter(4, [30, 400], btype='band', fs=SR, output='sos')
    dx = sosfiltfilt(sos, x)[::24]
    dy = sosfiltfilt(sos, y)[::24]
    n = 1 << int(math.ceil(math.log2(len(dx) + len(dy))))
    X, Y = np.fft.rfft(dx, n), np.fft.rfft(dy, n)
    Rxy = X * np.conj(Y)
    cc = np.fft.irfft(Rxy / (np.abs(Rxy) + 1e-9 * np.abs(Rxy).max()), n)
    lags = np.where(np.arange(n) < n // 2, np.arange(n), np.arange(n) - n) / 2000
    keep = (lags > -len(dy) / 2000) & (lags < len(dx) / 2000)
    cc, lags = cc[keep], lags[keep]
    i = int(np.argmax(cc))
    far = np.abs(lags - lags[i]) > .1
    j = int(np.argmax(np.where(far, cc, -1)))
    onset = int(np.argmax(np.abs(cache[clips[0]['file_path']]) > .1 * np.abs(cache[clips[0]['file_path']]).max())) / SR
    return float(lags[i]), float(cc[i]), float(lags[j]), float(cc[j]), onset


def band_rise(x, lo, hi):
    y = sosfilt(butter(4, [lo, hi], btype='band', fs=SR, output='sos'), x)
    n = len(y) // 48
    e = 10 * np.log10(np.mean(y[:n * 48].reshape(n, 48) ** 2, axis=1) + 1e-12)
    r = np.zeros(n)
    r[10:] = e[10:] - e[:-10]
    return np.maximum(r, 0)


def band_amp_rise(x, lo, hi):
    """10 ms rise of the band RMS amplitude, in units of the band's 99.9th-percentile amplitude (level-aware)."""
    y = sosfilt(butter(4, [lo, hi], btype='band', fs=SR, output='sos'), x)
    n = len(y) // 48
    a = np.sqrt(np.mean(y[:n * 48].reshape(n, 48) ** 2, axis=1))
    a = a / (np.percentile(a, 99.9) + 1e-12)
    r = np.zeros(n)
    r[10:] = a[10:] - a[:-10]
    return np.maximum(r, 0)


def onset_env(x):
    """Kick onset strength: level-aware 10 ms amplitude rise in 40-150 Hz plus 150-400 Hz, max-filtered over +-3 ms.
    The 40-150 band alone peaks on sidechained sub returning ~250 ms after the kick in full mixes."""
    return 100 * (maximum_filter1d(band_amp_rise(x, 40, 150), 7) + maximum_filter1d(band_amp_rise(x, 150, 400), 7))


def vals(env, t):
    return env[np.clip(np.round(t * 1000).astype(int), 0, len(env) - 1)]


def grid_search(env, notes_s, dur, spb):
    """Whole-bar export starts x one lag (+-80 ms); score = mean onset strength at the kick notes."""
    cands = []
    for k in range(0, int(notes_s.max() / spb / 4) + 2):
        t = notes_s - 4 * k * spb
        cands.append((k, (t >= .6) & (t <= dur - .6)))
    nmax = max(m.sum() for _, m in cands)
    best = {}
    for k, m in cands:
        if m.sum() < max(8, .9 * nmax):
            continue
        t = notes_s[m] - 4 * k * spb
        idx = np.round(t * 1000).astype(int)[:, None] + np.round(LAGS * 1000).astype(int)[None, :]
        sc = env[idx].mean(axis=0)
        i = int(np.argmax(sc))
        best[k] = (float(sc[i]), float(LAGS[i]))
    order = sorted(best, key=lambda k: -best[k][0])
    return best, order


def grid_corr(wide, notes_b, dur, spb):
    """Whole-bar export starts x one lag (+-80 ms); score = Pearson r between the kick pattern on the
    16th-note grid (1 where a note sits) and the mix onset strength at every grid point of the WIP."""
    pat_all = set(np.round(notes_b * 4).astype(int).tolist())
    kmax = int(notes_b.max() / 4) + 2
    best = {}
    lag_ms = np.round(LAGS * 1000).astype(int)
    for k in range(0, kmax + 1):
        g = np.arange(16 * k, 16 * k + int(dur / spb * 4) + 1)
        t = g / 4 * spb - 4 * k * spb
        m = (t >= .6) & (t <= dur - .6)
        g, t = g[m], t[m]
        p = np.array([1.0 if q in pat_all else 0.0 for q in g])
        if p.sum() < 8:
            continue
        idx = np.round(t * 1000).astype(int)[None, :] + lag_ms[:, None]
        v = wide[np.clip(idx, 0, len(wide) - 1)]
        v = v - v.mean(axis=1, keepdims=True)
        pc = p - p.mean()
        r = (v @ pc) / (np.linalg.norm(v, axis=1) * np.linalg.norm(pc) + 1e-12)
        i = int(np.argmax(r))
        best[k] = (float(r[i]), float(LAGS[i]))
    order = sorted(best, key=lambda k: -best[k][0])
    return best, order


def activity(res, nbeats):
    """Per arrangement beat: how many unmuted tracks have a clip playing (MIDI clips with notes, audio clips)."""
    muted = muted_map(res)
    a = np.zeros(nbeats + 64)
    for c in res['audio_clips']:
        if not c['disabled'] and not muted.get(c['track']):
            a[int(c['start_beat']):int(math.ceil(c['end_beat']))] += 1
    for t in res['tracks']:
        if t['kind'] != 'MidiTrack' or muted.get(t['name']) or re.search('resolume', t['name'], re.I):
            continue
        for c in t['midi_clips']:
            if c['notes']:
                a[int(c['start_beat']):int(math.ceil(c['end_beat']))] += 1
    return a


def beat_loudness(x, spb, k0_beats, nb):
    """Mix loudness (dB) of each beat, for a WIP whose start sits at arrangement beat k0_beats (unused: WIP-relative)."""
    n = int(spb * SR)
    m = len(x) // n
    return 10 * np.log10(np.mean(x[:m * n].reshape(m, n) ** 2, axis=1) + 1e-10)


def structure_scan(res, x, dur, spb, ks):
    """Pearson r between arrangement activity and mix loudness per beat, for each whole-bar export start k."""
    nb = int(dur / spb)
    act = activity(res, 4 * (max(ks) + 1) + nb)
    out = {}
    for k in ks:
        # WIP beat i <-> arrangement beat 4k + i; sub-beat lag is irrelevant at this scale
        lo = 10 * np.log10(np.array([np.mean(x[int(i * spb * SR):int((i + 1) * spb * SR)] ** 2) + 1e-10 for i in range(nb)])) \
            if k == ks[0] else lo
        a = act[4 * k:4 * k + nb]
        out[k] = float(np.corrcoef(a, lo)[0, 1]) if a.std() > 0 else -1.0
    return out


def all_onset_beats(res, sources):
    """16th-grid indices of every onset in the arrangement other than the kick: unmuted MIDI notes and audio clip starts."""
    muted = muted_map(res)
    src = {s.split(':', 1)[-1] for s in sources}
    q = []
    for c in res['audio_clips']:
        if not c['disabled'] and not muted.get(c['track']) and c['track'] not in src:
            q.append(c['start_beat'])
    for t in res['tracks']:
        if t['kind'] != 'MidiTrack' or muted.get(t['name']) or t['name'] in src or re.search('resolume', t['name'], re.I):
            continue
        q.extend(n['beat'] for c in t['midi_clips'] for n in c['notes'])
    g, cnt = np.unique(np.round(np.array(q) * 4).astype(int), return_counts=True)
    return dict(zip(g.tolist(), cnt.tolist()))


def music_scan(res, sources, x, dur, spb, ks, lag):
    """Pearson r between the non-kick onset pattern (16th grid, onset counts) and the mix's 150-8000 Hz onset strength."""
    pat = all_onset_beats(res, sources)
    env = maximum_filter1d(band_rise(x, 150, 8000), 21)
    out = {}
    for k in ks:
        g = np.arange(16 * k, 16 * k + int(dur / spb * 4) + 1)
        t = g / 4 * spb - 4 * k * spb + lag
        m = (t >= .6) & (t <= dur - .6)
        p = np.array([pat.get(q, 0) for q in g[m]], dtype=float)
        v = env[np.round(t[m] * 1000).astype(int)]
        out[k] = float(np.corrcoef(p, v)[0, 1]) if p.std() > 0 else -1.0
    return out


def paired_t(env, notes_s, dur, o0, o1):
    t0, t1 = notes_s + o0, notes_s + o1
    m = (t0 >= .6) & (t0 <= dur - .6) & (t1 >= .6) & (t1 <= dur - .6)
    d = vals(env, t0[m]) - vals(env, t1[m])
    return float(d.mean() / (d.std(ddof=1) / math.sqrt(len(d)) + 1e-9))


def run(name):
    wip, js, sources = PROJECTS[name]
    res = json.load(open(C / f'{js}.json'))
    assert len(res['tempo_points']) == 1, 'tempo automation not handled'
    bpm = res['tempo_points'][0]['bpm']
    spb = 60 / bpm
    x = load(wip)
    dur = len(x) / SR
    env = onset_env(x)
    wide = maximum_filter1d(env, 2 * int(HIT * 1000) + 1)
    notes_s, notes_b, clips = kick_notes(res, sources)
    out = dict(project=name, wip=wip, als=res['source']['path'], als_sha256=res['source']['sha256'], bpm=bpm,
               duration_s=round(dur, 3), kick_sources=sources)
    if True:
        best, order = grid_corr(wide, notes_b, dur, spb)
        k0, k1 = order[0], order[1]
        s0, lag = best[k0]
        tk = notes_s - 4 * k0 * spb
        tk = tk[(tk >= .6) & (tk <= dur - .6)]
        lag = float(LAGS[int(np.argmax([vals(env, tk + lg).mean() for lg in LAGS]))])  # refine on the unwidened envelope
        lag -= ENV_BIAS
        off = -4 * k0 * spb + lag
        rs = np.array([best[k][0] for k in order])
        z_margin = (s0 - best[k1][0]) / (rs.std() + 1e-9)
        off1 = -4 * k1 * spb + lag
        out["method"] = "whole-bar grid + lag, Pearson r of 16th-grid kick pattern vs mix onset strength"
        out['grid'] = [dict(start_beat=4 * k, lag_ms=round(best[k][1] * 1000), r=round(best[k][0], 3)) for k in order[:4]]
        out["margin"] = dict(r_diff=round(s0 - best[k1][0], 3), r_diff_in_sd=round(float(z_margin), 2), paired_t=round(paired_t(env, notes_s, dur, off, off1), 1),
                             runner_up_start_beat=4 * k1)
    if clips:
        l0, c0, l1, c1, onset = rebuild_xcorr(x, clips, dur)
        grid_off = off
        off = l0 + onset
        out['method'] = 'audio rebuild GCC-PHAT cross-correlation (grid method kept as a cross-check)'
        out['xcorr'] = dict(sample_offset_s=round(l0, 4), peak=round(c0, 4), runner_up_offset_s=round(l1, 4),
                            runner_up_peak=round(c1, 4), sample_onset_s=round(onset, 4), grid_offset_s=round(grid_off, 4),
                            grid_minus_xcorr_ms=round((grid_off - off) * 1000, 1))
        out['margin']['xcorr_peak_ratio'] = round(c0 / c1, 2)
        lag = off + 4 * k0 * spb
    out.update(offset_s=round(off, 4), offset_beats=round(off / spb, 3), lag_ms=None if lag is None else round(lag * 1000))
    t_all = notes_s + off
    inside = (t_all >= .6) & (t_all <= dur - .6)
    # a note is on a hit when the onset strength within +-20 ms reaches the song's 95th percentile
    thr = float(np.percentile(env, 95))
    on = vals(wide, t_all) >= thr
    out.update(notes_total=int(len(notes_s)), notes_inside=int(inside.sum()), hit_threshold=round(thr, 1),
               notes_on_hit_share=round(float(on[inside].mean()), 3))
    # kick-like onset: a combined-onset peak where the 40-150 Hz and the 150-400 Hz rises (max within +-5 ms) both reach
    # the weakest quartile of their values at matched kicks. Bass rises only low, snare bodies only mid.
    lowe = maximum_filter1d(100 * maximum_filter1d(band_amp_rise(x, 40, 150), 7), 11)
    mide = maximum_filter1d(100 * maximum_filter1d(band_amp_rise(x, 150, 400), 7), 11)
    tk = t_all[inside & on]
    kl_thr = [float(np.percentile(vals(e, tk), 25)) if len(tk) else float(np.percentile(e, 99)) for e in (lowe, mide)]
    pk = np.flatnonzero((env[1:-1] >= env[:-2]) & (env[1:-1] > env[2:]) & (lowe[1:-1] >= kl_thr[0]) & (mide[1:-1] >= kl_thr[1])) + 1
    peaks, last = [], -10 ** 9
    for i in pk:
        if i - last > 80:
            peaks.append(i / 1000)
            last = i
    peaks = np.array(peaks)
    others = other_drums(res, sources)
    loop_kicks = {f: loop_kick_rate(f) for f in sorted({o[3] for o in others if o[3]})}
    out["drum_loop_kick_rates"] = {Path(f).name: r for f, r in loop_kicks.items()}
    shifts = np.arange(.06, spb - .06, .02)
    sections, kicks, spans, loop_kicks_s, loop_spans = [], [], [], [], []
    unmarked = peaks[nearest(peaks, np.sort(t_all)) > .04] if len(peaks) else peaks
    for j in range(int(math.floor(-off / spb / SEC_BEATS)), int(math.ceil((dur - off) / spb / SEC_BEATS)) + 1):
        b0, b1 = j * SEC_BEATS, (j + 1) * SEC_BEATS
        s0, s1 = b0 * spb + off, b1 * spb + off
        if s0 < 0 or s1 > dur:
            continue
        sec = dict(bars=[b0 // 4 + 1, b1 // 4], wip_s=[round(s0, 3), round(s1, 3)])
        od = sorted({lab for a, b, lab, f in others if a < b1 - .5 and b > b0 + .5 and loop_kicks.get(f, 1.0) >= LOOP_KICK_RATE})
        sel = (notes_b >= b0) & (notes_b < b1)
        n = int(sel.sum())
        npk = int(np.sum((peaks >= s0) & (peaks < s1)))
        nun = int(np.sum((unmarked >= s0) & (unmarked < s1)))
        sec.update(notes=n, kick_like_peaks=npk, other_drums=od[:6])
        if n:
            t = t_all[sel]
            share = float(np.mean(vals(wide, t) >= thr))
            chance = float(np.mean([np.mean(vals(wide, t + s) >= thr) for s in shifts]))
            sec.update(on_hit=round(share, 3), chance=round(chance, 3), unmarked_kick_like=nun)
            passed = n >= 4 and share >= .8 and share - chance >= .4
            if od:
                sec['status'] = 'other_drums'
                if passed and nun <= UNMARKED_MAX:
                    sec['status'] = 'other_drums_but_clean'
                    loop_kicks_s.extend(t.tolist())
                    loop_spans.append([round(s0, 3), round(s1, 3)])
            elif passed and nun <= UNMARKED_MAX:
                sec['status'] = 'usable_kick'
                kicks.extend(t.tolist())
                spans.append([round(s0, 3), round(s1, 3)])
            else:
                sec['status'] = 'kick_fail'
        elif od:
            sec['status'] = 'other_drums'
        elif npk <= 2:
            sec['status'] = 'usable_kick_free'
            spans.append([round(s0, 3), round(s1, 3)])
        else:
            sec['status'] = 'kick_free_with_low_hits'
        sections.append(sec)
    out.update(other_drums_but_clean_spans_s=loop_spans, kicks_in_other_drums_but_clean_s=[round(k, 4) for k in sorted(loop_kicks_s)])
    out.update(kick_like_threshold=[round(v, 1) for v in kl_thr], kicks_s=[round(k, 4) for k in sorted(kicks)], scored_spans_s=spans,
               sections=sections, usable_s=round(sum(b - a for a, b in spans), 1),
               usable_kick_s=round(sum(s['wip_s'][1] - s['wip_s'][0] for s in sections if s['status'] == 'usable_kick'), 1))
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / f'{name}.json').write_text(json.dumps(out, indent=1))
    st = ''.join({'usable_kick': 'K', 'usable_kick_free': 'n', 'kick_fail': 'x', 'other_drums': 'd',
                  'kick_free_with_low_hits': 'h', 'other_drums_but_clean': 'c'}[s['status']] for s in sections)
    print(json.dumps(dict(project=name, offset_beats=out["offset_beats"], lag_ms=out['lag_ms'], grid=out.get('grid'),
                          xcorr=out.get('xcorr'), margin=out['margin'], on_hit=out['notes_on_hit_share'], sections=st,
                          usable_min=round(out['usable_s'] / 60, 2), kick_min=round(out['usable_kick_s'] / 60, 2),
                          kicks=len(kicks), loopclean_min=round(sum(b - a for a, b in loop_spans) / 60, 2), loopclean_kicks=len(loop_kicks_s))), flush=True)


if __name__ == '__main__':
    for p in sys.argv[1:]:
        run(p)
