#!/usr/bin/env python3
"""Extract kick event times (seconds) from drum stems; render verification PNGs.

Ground-truth path for the BUG-046 sweep-event detector: labels come from the
isolated drums stem (low-band energy onsets), verified by eye on a spectrogram
of BOTH the drums stem and the mix (alignment check), then frozen as CSV.
"""
import sys, os, csv
import numpy as np
from scipy.io import wavfile
from scipy.signal import butter, sosfilt, stft
from PIL import Image, ImageDraw

FIXTURES = "tests/fixtures/audio"
TRACKS = ["apricots_128bpm", "bad_guy_128bpm", "feel_the_vibration_174bpm",
          "inhale_exhale_145bpm", "tears_140bpm"]
OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/kick_labels"
os.makedirs(OUT, exist_ok=True)

HOP_S = 0.005          # 5 ms envelope hop
MIN_SEP_S = 0.08       # min gap between kick peaks
ABS_GATE = 0.10        # fraction of p99 envelope peak
SUB_GATE = 0.5         # kick = sub(30-90) env peak > this fraction of the track's p99
# Bimodal on all 5 tracks: snaps/snares reach 0.25-0.43 of kick sub (bad_guy),
# 0.62-0.67 (apricots snare-coincident? checked by eye), kicks sit at 0.97-1.24.
SUB_DOMINANCE = SUB_GATE  # annotation color threshold (r is peak/p99 now)

def mono(path):
    sr, d = wavfile.read(path)
    if d.dtype == np.int16: d = d / 32768.0
    elif d.dtype == np.int32: d = d / 2147483648.0
    d = d.astype(np.float64)
    if d.ndim == 2: d = d.mean(axis=1)
    return sr, d

def band_env(sr, x, lo, hi):
    sos = butter(4, [lo, hi], btype="bandpass", fs=sr, output="sos")
    b = sosfilt(sos, x)
    hop = int(sr * HOP_S)
    n = len(b) // hop
    return np.sqrt(np.mean(b[: n * hop].reshape(n, hop) ** 2, axis=1))

def kick_onsets(sr, x):
    """Peak-pick the SUB (30-90Hz) envelope of an ISOLATED drum stem. A kick is
    an event with strong absolute sub energy — dominance ratios fail when a kick
    and snare land together (body inflates, ratio sinks, kick still there).
    Onset = walk-back to 25% of peak. Annotation ratio kept for diagnostics."""
    sub = band_env(sr, x, 30, 90)
    body = band_env(sr, x, 150, 300)
    gate = np.percentile(sub, 99) * SUB_GATE
    sep = int(MIN_SEP_S / HOP_S)
    fires, ratios = [], []
    last = -sep
    for i in range(1, len(sub) - 1):
        if sub[i] != sub[max(0, i - sep) : i + sep].max() or i - last < sep:
            continue
        r = sub[i] / (np.percentile(sub, 99) + 1e-9)  # peak strength vs track's loud kicks
        if sub[i] <= np.percentile(sub, 99) * 0.02:
            continue  # skip silence-floor local maxima entirely
        ratios.append((round(i * HOP_S, 3), round(r, 2)))
        if sub[i] <= gate:
            continue
        j = i
        while j > 0 and sub[j - 1] < sub[j] and sub[j - 1] > 0.25 * sub[i]:
            j -= 1
        fires.append(j * HOP_S)
        last = i
    return sub, fires, ratios

def spec_panel(sr, x, fmax=300.0, px_per_s=100, height=220):
    f, t, Z = stft(x, fs=sr, nperseg=2048, noverlap=2048 - 512)
    Z = np.abs(Z[f <= fmax, :])
    db = 20 * np.log10(Z + 1e-9)
    db = np.clip((db - db.max() + 60) / 60, 0, 1)  # 60 dB range
    img = (db[::-1, :] * 255).astype(np.uint8)     # low freq at bottom
    im = Image.fromarray(img, "L").convert("RGB")
    w = int(t[-1] * px_per_s)
    return im.resize((w, height)), t[-1]

def mix_env(sr, x):
    sos = butter(4, [30, 150], btype="bandpass", fs=sr, output="sos")
    low = sosfilt(sos, x)
    hop = int(sr * HOP_S)
    n = len(low) // hop
    return np.sqrt(np.mean(low[: n * hop].reshape(n, hop) ** 2, axis=1))

def snap_to_mix_onset(t, menv, win_s=0.06):
    """Snap a predicted time to the strongest local low-band env rise in the mix."""
    i0 = int((t - win_s) / HOP_S); i1 = int((t + win_s) / HOP_S)
    i0 = max(i0, 4); i1 = min(i1, len(menv) - 1)
    if i1 <= i0: return t
    d = menv[i0:i1] - menv[i0 - 4 : i1 - 4]  # rise over 20 ms
    return (i0 + int(np.argmax(d))) * HOP_S

def render(track, panels, fires_mixtime, env, path, ratios=(), k=1.0):
    # panels: list of (image, caption); fires drawn in mix time on all
    w = max(p.width for p, _ in panels)
    env_h, gap = 60, 14
    H = sum(p.height for p, _ in panels) + env_h + (len(panels) + 1) * gap
    canvas = Image.new("RGB", (w, H), (12, 12, 12))
    d = ImageDraw.Draw(canvas)
    y = gap
    caps = []
    for idx, (p, cap) in enumerate(panels):
        canvas.paste(p, (0, y)); caps.append((cap, y)); y += p.height + gap
        if idx == 0:  # envelope lane under first panel
            e = env / (env.max() + 1e-9)
            for i in range(len(e) - 1):
                x0 = int(i * HOP_S * 100)
                if x0 >= w: break
                d.line([(x0, y + env_h - 8 - e[i] * (env_h - 12)),
                        (x0 + 1, y + env_h - 8 - e[i + 1] * (env_h - 12))], fill=(90, 200, 90))
            y += env_h
    for t in fires_mixtime:
        x = int(t * 100)
        d.line([(x, 0), (x, H)], fill=(255, 60, 60), width=2)
    for cap, cy in caps:
        d.text((4, cy - 12), cap, fill=(255, 200, 80))
    for t, r in ratios:  # sub/body ratio at every candidate peak (drums time -> mix time)
        x = int(t * k * 100)
        col = (120, 255, 120) if r >= SUB_DOMINANCE else (255, 255, 100)
        d.text((max(x - 10, 0), panels[0][0].height + 2), f"{r:.1f}", fill=col)
    canvas.save(path)

for track in TRACKS:
    sr_d, drums = mono(f"{FIXTURES}/{track}/drums.wav")
    sr_m, mix = mono(f"{FIXTURES}/{track}/mix.wav")
    env, fires, ratios = kick_onsets(sr_d, drums)
    dur_d, dur_m = len(drums) / sr_d, len(mix) / sr_m
    k = dur_m / dur_d  # bad_guy: stems unwarped (15.0s) vs warped mix (13.241s)
    menv = mix_env(sr_m, mix)
    if abs(k - 1.0) > 0.001:
        fires_mix = [snap_to_mix_onset(t * k, menv) for t in fires]
        fires_mix = [t for t in fires_mix if t < dur_m]
    else:
        fires_mix = fires
    with open(f"{OUT}/{track}.csv", "w", newline="") as fh:
        wtr = csv.writer(fh); wtr.writerow(["mix_time_s", "drums_time_s"])
        for tm, td in zip(fires_mix, fires): wtr.writerow([f"{tm:.3f}", f"{td:.3f}"])
    pfull, _ = spec_panel(sr_d, drums, fmax=8000.0, height=180)
    plow, _ = spec_panel(sr_d, drums, fmax=300.0)
    pmix, _ = spec_panel(sr_m, mix, fmax=300.0)
    if abs(k - 1.0) > 0.001:  # draw drum panels in mix time so ticks line up
        pfull = pfull.resize((int(pfull.width * k), pfull.height))
        plow = plow.resize((int(plow.width * k), plow.height))
        env = np.interp(np.arange(int(len(env) * k)) / k, np.arange(len(env)), env)
    render(track, [(pfull, f"{track} drums FULL 0-8k | {len(fires_mix)} kicks | red=label (mix time)"),
                   (plow, "drums LOW 0-300"), (pmix, "mix LOW 0-300 (alignment check)")],
           fires_mix, env, f"{OUT}/{track}.png", ratios=ratios, k=k)
    rs = sorted(r for _, r in ratios)
    print(f"{track}: {len(fires_mix)} kicks of {len(ratios)} peaks | k={k:.4f} | "
          f"sub/body sorted: {rs}")
