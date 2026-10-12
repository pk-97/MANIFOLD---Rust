#!/usr/bin/env python3
"""Remix renderer for detector training data: a labelled song rebuilt from its own stems with a random balance and
master chain, so the labels (hit times from MIDI) stay exact. Prep only: nothing here is wired into training.

Stems come from the catalogue (catalog.stems). Everything is mono float64 at the song's rate on the lab mix's
timeline: the stem sum is aligned to registry.mix by stem_lag plus a sample-level refinement.

Recipe (RECIPE_VERSION), all seeded: a gain per role (drums, kick, bass, nodrums/nokick as one; every other stem on its
own) uniform +-6 dB; low and high shelf tilt +-3 dB; feed-forward RMS bus compressor (ratio 1.5-4, threshold within
+-4 dB of the bus RMS, attack 5-30 ms, release 50-300 ms); drive 0-6 dB into a lookahead peak limiter (5 ms) at -1 dBFS.
Each stem's gain depends only on (seed, its group), so adding a stem never re-rolls the others.

Unity check: the stems summed at 0 dB, no chain, against the lab mix: lag, correlation, residual (dB under the mix,
at the best single gain and at gain 1), and a verdict. A song whose residual is worse than -20 dB is flagged, not used.

Usage: remix.py unity [SONG...] | render SONG SEED | preview SONG... [--seeds 0 1 2] [--out DIR] [--secs 30]
The music folder is read only: run under the sandbox-exec wrapper in catalog.py's docstring."""
import hashlib
import json
import sys
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402
from scipy.ndimage import minimum_filter1d, uniform_filter1d  # noqa: E402
from scipy.signal import sosfilt  # noqa: E402

RECIPE_VERSION = 'remix-1'
UNITY_VERSION = 'unity-1'
CACHE = Path.home() / '.cache/manifold/detector-lab/remix'
BAND = (100, 8000)           # the alignment band (Hz)
LAG_Z_MIN = 8.0
FLAG_DB = -20.0              # residual worse than this flags the song
CEILING_DB = -1.0
LOOKAHEAD_S = .005


# ---- the chain ----------------------------------------------------------------------------------------------------

@dataclass
class Part:
    name: str
    role: str                # drums kick bass other nodrums nokick
    load: Callable[[], np.ndarray]   # aligned, the song's length


def group_of(p):
    return p.role if p.role != 'other' else f'other:{p.name}'


def _rng(seed, tag):
    return np.random.default_rng([int(seed), zlib.crc32(tag.encode())])


def draw(seed, groups):
    """All of a seed's random choices: a gain per group, and the chain."""
    gains = {g: float(_rng(seed, 'gain:' + g).uniform(-6, 6)) for g in sorted(groups)}
    r = _rng(seed, 'chain')
    chain = dict(low_db=r.uniform(-3, 3), low_hz=r.uniform(100, 300), high_db=r.uniform(-3, 3), high_hz=r.uniform(3000, 8000),
                 ratio=r.uniform(1.5, 4), thr_off_db=r.uniform(-4, 4), attack_s=r.uniform(.005, .030),
                 release_s=r.uniform(.050, .300), drive_db=r.uniform(0, 6))
    return gains, {k: float(v) for k, v in chain.items()}


def shelf_sos(kind, f0, gain_db, fs):
    """RBJ shelf (slope 1) as one second-order section."""
    A = 10 ** (gain_db / 40)
    w = 2 * np.pi * f0 / fs
    cw, sw = np.cos(w), np.sin(w)
    al = sw / 2 * np.sqrt(2.0)
    sq = 2 * np.sqrt(A) * al
    if kind == 'low':
        b = [A * ((A + 1) - (A - 1) * cw + sq), 2 * A * ((A - 1) - (A + 1) * cw), A * ((A + 1) - (A - 1) * cw - sq)]
        a = [(A + 1) + (A - 1) * cw + sq, -2 * ((A - 1) + (A + 1) * cw), (A + 1) + (A - 1) * cw - sq]
    else:
        b = [A * ((A + 1) + (A - 1) * cw + sq), -2 * A * ((A - 1) + (A + 1) * cw), A * ((A + 1) + (A - 1) * cw - sq)]
        a = [(A + 1) - (A - 1) * cw + sq, 2 * ((A - 1) - (A + 1) * cw), (A + 1) - (A - 1) * cw - sq]
    return np.array([[b[0] / a[0], b[1] / a[0], b[2] / a[0], 1.0, a[1] / a[0], a[2] / a[0]]])


def tilt(x, sr, c):
    y = sosfilt(shelf_sos('low', c['low_hz'], c['low_db'], sr), x)
    return sosfilt(shelf_sos('high', c['high_hz'], c['high_db'], sr), y)


def gated_rms_db(x, sr):
    """RMS of the bus over its 100 ms frames within 40 dB of the loudest, so silence does not set the threshold."""
    hop = sr // 10
    n = len(x) // hop
    p = (x[:n * hop] ** 2).reshape(n, hop).mean(axis=1)
    keep = p > p.max() * 1e-4
    return float(10 * np.log10(p[keep].mean() + 1e-20))


def compress(x, sr, thr_db, ratio, attack_s, release_s):
    """Feed-forward RMS compressor: 10 ms RMS read on a 1 kHz control grid, gain computer, attack/release smoothing of
    the reduction, gain interpolated back to the sample rate."""
    hop = max(1, sr // 1000)
    n = len(x)
    c = np.concatenate([[0.0], np.cumsum(x * x)])
    ends = np.minimum(np.arange(hop, n + hop, hop), n)
    w = int(.010 * sr)
    starts = np.maximum(ends - w, 0)
    ms = (c[ends] - c[starts]) / np.maximum(ends - starts, 1)
    lvl = 10 * np.log10(ms + 1e-20)
    target = np.maximum(lvl - thr_db, 0.0) * (1 - 1 / ratio)
    ka, kr = np.exp(-hop / (sr * attack_s)), np.exp(-hop / (sr * release_s))
    gr = np.empty(len(target))
    g = 0.0
    for i, t in enumerate(target.tolist()):
        k = ka if t > g else kr
        g = k * g + (1 - k) * t
        gr[i] = g
    centres = ends - (ends - starts) / 2
    gain = 10 ** (-np.interp(np.arange(n), centres, gr) / 20)
    return x * gain


def limit(x, sr, ceiling_db=CEILING_DB, lookahead_s=LOOKAHEAD_S):
    """Lookahead peak limiter. The gain at n is the mean of L window-minimums of the needed gain, each looking L-1
    samples ahead, so |y[n]| <= ceiling holds exactly while the gain ramps over the L samples before a peak."""
    ceil = 10 ** (ceiling_db / 20)
    L = max(2, int(lookahead_s * sr) - 1)
    need = np.minimum(1.0, ceil / np.maximum(np.abs(x), 1e-12))
    m = minimum_filter1d(need, size=L, origin=-(L // 2), mode='constant', cval=1.0)
    g = uniform_filter1d(m, size=L, origin=(L - 1) // 2, mode='constant', cval=1.0)
    return np.clip(x * g, -ceil, ceil)


def chain(x, sr, c):
    y = tilt(x, sr, c)
    y = compress(y, sr, gated_rms_db(y, sr) + c['thr_off_db'], c['ratio'], c['attack_s'], c['release_s'])
    return limit(y * 10 ** (c['drive_db'] / 20), sr)


def render_parts(parts, n, sr, seed, unity=False, with_chain=True):
    """The mix of parts for a seed. unity=True: every gain 0 dB, no chain (the plain sum). Returns (audio, params)."""
    groups = sorted({group_of(p) for p in parts})
    gains, c = draw(seed, groups)
    if unity:
        gains, with_chain = {g: 0.0 for g in groups}, False
    out = np.zeros(n)
    for p in parts:
        out += p.load()[:n] * 10 ** (gains[group_of(p)] / 20)
    if with_chain:
        out = chain(out, sr, c)
    return out, dict(gains_db=gains, chain=c if with_chain else None)


# ---- loading, alignment, unity -------------------------------------------------------------------------------------

def read_mono(path, sr):
    from tools.audio_analysis.eval.detector_labels import read_any
    from tools.audio_analysis.eval.kick_attack_rejection import read_audio
    return read_any(str(path), sr, lambda p, s: read_audio(p, s)[1])


def shift(x, k, n):
    """x delayed by k samples (advanced when negative), cut or padded to n."""
    out = np.zeros(n)
    if k >= 0:
        m = max(0, min(len(x), n - k))
        out[k:k + m] = x[:m]
    else:
        m = max(0, min(len(x) + k, n))
        out[:m] = x[-k:-k + m]
    return out


def refine_lag(mix, s, k0, sr, span=96, win_s=8):
    """Sample-level lag near k0: the shift of s that best matches mix over the loudest 8 s of the mix."""
    w = int(win_s * sr)
    hop = sr
    n = len(mix)
    if n <= w:
        a, b = 0, n
    else:
        e = np.array([np.sum(mix[i:i + w] ** 2) for i in range(0, n - w, hop)])
        a = int(np.argmax(e)) * hop
        b = a + w
    seg = mix[a:b]
    best, bk = -np.inf, k0
    for k in range(k0 - span, k0 + span + 1):
        sh = shift(s, k, n)[a:b]
        v = float(np.dot(seg, sh))
        if v > best:
            best, bk = v, k
    return bk


def stem_info(song_id):
    """{path: (role, sha256)} of the catalogue's stem set."""
    from tools.audio_analysis.catalog import catalog
    st = catalog.stems(song_id)
    return {str(p): (role, catalog.stem_sha(p)) for role, ps in st.items() for p in ps}


def _key(song, info, *extra):
    h = hashlib.sha256(json.dumps([song.content_id, sorted((p, v[1]) for p, v in info.items()), *extra]).encode())
    return h.hexdigest()[:24]


def unity(song, force=False, fit=True):
    """The unity check for a song (cached). Returns the report dict (also what catalog.py build ingests)."""
    from tools.audio_analysis.eval.detector_labels import stem_lag
    from tools.audio_analysis.lab import registry
    info = stem_info(song.id)
    if not info:
        return dict(song=song.id, verdict='no stems', residual_db=None, detail={'reason': 'no stem set in the catalogue'})
    key = _key(song, info, UNITY_VERSION)
    path = CACHE / 'unity' / f'{song.id}.json'
    if path.exists() and not force:
        r = json.loads(path.read_text())
        if r.get('key') == key:
            return r
    sr = song.sample_rate
    mix = registry.mix(song)
    n = len(mix)
    arrs = {p: read_mono(p, sr).astype(np.float32) for p in info}
    total = np.zeros(max(len(a) for a in arrs.values()))
    for a in arrs.values():
        total[:len(a)] += a
    L, z = stem_lag(total, mix, sr, BAND)
    k = refine_lag(mix, total, int(round(L * sr)), sr)
    s = shift(total, k, n)
    g = float(np.dot(mix, s) / max(np.dot(s, s), 1e-30))
    corr = float(np.corrcoef(mix, s)[0, 1]) if s.any() else 0.0
    pm = float(np.dot(mix, mix))
    res = lambda a: float(10 * np.log10(max(np.dot(mix - a, mix - a), 1e-30) / pm))  # noqa: E731
    r = dict(song=song.id, key=key, lag_s=k / sr, lag_samples=int(k), lag_z=round(z, 1), corr=round(corr, 5),
             residual_db=round(res(g * s), 2), raw_residual_db=round(res(s), 2), gain_db=round(20 * np.log10(abs(g) + 1e-30), 2),
             stems={Path(p).name: v[0] for p, v in info.items()}, detail={})
    reasons = []
    if r['residual_db'] > FLAG_DB:
        reasons.append(f"residual {r['residual_db']} dB is worse than {FLAG_DB}")
    if z < LAG_Z_MIN and abs(k) > 0:
        reasons.append(f'lag z {z:.1f} is under {LAG_Z_MIN}')
    r['verdict'] = 'flagged' if reasons else 'usable'
    r['detail']['aligned'] = bool(z >= LAG_Z_MIN and corr >= .85)      # a flagged song can still be timed right (mastering chain on top)
    r['detail']['reasons'] = reasons
    if fit and len(arrs) * n <= 4e8:
        S = np.stack([shift(a, k, n).astype(np.float32) for a in arrs.values()])      # one float32 stack: the fit is advisory
        gram = np.zeros((len(S), len(S)))
        rhs = np.zeros(len(S))
        for a in range(0, n, 1 << 20):                  # float64 sums over chunks; a float32 matmul over 10M samples is too coarse
            c = S[:, a:a + (1 << 20)].astype(np.float64)
            gram += c @ c.T
            rhs += c @ mix[a:a + (1 << 20)]
        w = np.linalg.lstsq(gram + 1e-9 * np.eye(len(gram)) * np.trace(gram) / len(gram), rhs, rcond=None)[0]
        fit_s = (w[:, None].astype(np.float32) * S).sum(axis=0).astype(np.float64)
        r['detail']['fit_residual_db'] = round(res(fit_s), 2)
        r['detail']['fit_gains_db'] = {Path(p).name: round(float(20 * np.log10(abs(x) + 1e-12)), 1) for p, x in zip(arrs, w)}
    (CACHE / 'unity').mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(r, indent=1, sort_keys=True))
    return r


# ---- rendering ----------------------------------------------------------------------------------------------------

def render(song, seed, force=False, allow_flagged=False):
    """Mono float64 at song.sample_rate on the lab mix's timeline, the seed's remix of the song's stems. Cached."""
    from tools.audio_analysis.lab import registry
    info = stem_info(song.id)
    if not info:
        raise ValueError(f'{song.id}: no stem set in the catalogue')
    u = unity(song, fit=False)
    if u['verdict'] != 'usable' and not (allow_flagged and u['detail'].get('aligned')):
        raise ValueError(f"{song.id}: unity check flagged ({'; '.join(u['detail']['reasons'])})")
    key = _key(song, info, int(seed), RECIPE_VERSION, u['lag_samples'])
    f = CACHE / 'render' / f'{key}.npy'
    if f.exists() and not force:
        return np.load(f).astype(np.float64)
    sr, k = song.sample_rate, u['lag_samples']
    n = len(registry.mix(song))
    parts = [Part(Path(p).name, v[0], (lambda p=p: shift(read_mono(p, sr), k, n))) for p, v in sorted(info.items())]
    out, params = render_parts(parts, n, sr, seed)
    out32 = out.astype(np.float32)
    f.parent.mkdir(parents=True, exist_ok=True)
    np.save(f, out32)
    sha = hashlib.sha256(out32.tobytes()).hexdigest()
    f.with_suffix('.json').write_text(json.dumps(dict(
        key=key, song=song.id, seed=int(seed), recipe_version=RECIPE_VERSION, path=str(f), sha256=sha,
        sources={p: v[1] for p, v in info.items()}, params=params), indent=1, sort_keys=True))
    return out32.astype(np.float64)


def unity_sum(song):
    """The plain stem sum on the lab mix's timeline (for listening)."""
    from tools.audio_analysis.lab import registry
    info = stem_info(song.id)
    u = unity(song, fit=False)
    sr, k, n = song.sample_rate, u['lag_samples'], len(registry.mix(song))
    out = np.zeros(n)
    for p in info:
        out += shift(read_mono(p, sr), k, n)
    return out


def pick_section(song, secs=30.0):
    """Start (s) of the busiest drum stretch: the window with the most seconds within 12 dB of the song's 90th
    percentile drum level, from the drums (else kick) stems."""
    from tools.audio_analysis.lab import registry
    info = stem_info(song.id)
    u = unity(song, fit=False)
    sr, k = song.sample_rate, u['lag_samples']
    N = len(registry.mix(song))
    for roles in (('drums',), ('kick',), ('nokick',)):
        paths = [p for p, v in info.items() if v[0] in roles]
        if paths:
            break
    x = sum(shift(read_mono(p, sr), k, N) for p in paths)
    n = int(len(x) // sr)
    db = 10 * np.log10((x[:n * sr] ** 2).reshape(n, sr).mean(axis=1) + 1e-12)
    busy = (db > np.percentile(db, 90) - 12).astype(float)
    w = int(secs)
    if n <= w:
        return 0.0
    score = np.convolve(busy, np.ones(w), 'valid') + 1e-3 * np.convolve(db, np.ones(w), 'valid') / w
    return float(int(np.argmax(score)))


def write_wav(path, x, sr):
    import soundfile as sf
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(path), x.astype(np.float32), sr, subtype='FLOAT')


def main():
    import argparse

    from tools.audio_analysis.lab import registry
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('cmd', choices=['unity', 'render', 'preview'])
    ap.add_argument('args', nargs='*')
    ap.add_argument('--seeds', nargs='*', type=int, default=[0, 1, 2])
    ap.add_argument('--out', default='/tmp/remix_preview')
    ap.add_argument('--secs', type=float, default=30.0)
    ap.add_argument('--force', action='store_true')
    a = ap.parse_args()
    songs = registry.load()
    if a.cmd == 'unity':
        from tools.audio_analysis.catalog import catalog
        ids = a.args or [t for t in songs if catalog.stems(t)]
        for t in ids:
            r = unity(songs[t], force=a.force)
            d = r.get('detail', {})
            print(f"{t:18s} {r['verdict']:8s} lag {r.get('lag_s', 0) * 1000:9.3f} ms  corr {r.get('corr')}  residual {r.get('residual_db')} dB"
                  f" (gain 1: {r.get('raw_residual_db')}, fit gain {r.get('gain_db')} dB)  free-gain fit {d.get('fit_residual_db')}  {d.get('reasons')}", flush=True)
    elif a.cmd == 'render':
        x = render(songs[a.args[0]], int(a.args[1]), force=a.force)
        print(a.args, len(x), float(np.abs(x).max()))
    else:
        for t in a.args:
            s = songs[t]
            start = pick_section(s, a.secs)
            lo, hi = int(start * s.sample_rate), int((start + a.secs) * s.sample_rate)
            write_wav(Path(a.out) / f'{t}_unity.wav', unity_sum(s)[lo:hi], s.sample_rate)
            for seed in a.seeds:
                x = render(s, seed, force=a.force)
                write_wav(Path(a.out) / f'{t}_{seed}.wav', x[lo:hi], s.sample_rate)
            print(f'{t}: section {start:.0f}-{start + a.secs:.0f} s', flush=True)


if __name__ == '__main__':
    main()
