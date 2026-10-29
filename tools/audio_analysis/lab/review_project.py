#!/usr/bin/env python3
"""A MANIFOLD project for reviewing detector labels by ear and eye (docs/DETECTOR_LABEL_REVIEW_DESIGN.md).

Every project song sits end to end on one timeline, each in its own tempo window that matches its Ableton set: the
bar the song's audio starts in lands on the window start, so every bar line is Ableton's. A song plays its latest
master when the lab proved that master carries its labels (lab/variant_proofs.json), shifted by the proven lag; else
the labelled file, with the reason on the song's start marker. Lanes: Mix, Drums (proven stems only), Kick labels,
Snare labels, Kick fires, Snare fires, Kick unsure, Snare unsure. Peter edits a copy; `read` diffs his copy against
the original by clip id and writes one overlay per family, in the labelled file's seconds. Nothing here writes a label
file.

Usage:
  review_project.py build OUT.manifold [--songs a,b] [--fires-dir DIR] [--dry-run] [--project-tool CMD]
  review_project.py fires DIR [--songs a,b] [--fires-cmd CMD] [--force]
  review_project.py read ORIGINAL.manifold EDITED.manifold [--project-tool CMD]

build writes the mix and drum WAVs (48 kHz, stereo where the source is) under ~/.cache/manifold/detector-lab/review/,
the project through project_tool, and OUT.review.json beside it (clip id -> song, family, kind, labelled-file time;
per song its audio source and shift). --dry-run prints the layout and per-lane counts and writes nothing. Fires are
an input: DIR/<song>.json, a JSON list or JSON lines of {"family", "onset_s", "emit_s"} in the seconds of the WAV
named in DIR/<song>.source; `fires DIR` makes them with the shipped detectors (manifold-audio's detector_fires
example, CPU) from the same WAVs, and build skips a song's fires made from another WAV."""
import argparse
import bisect
import hashlib
import json
import math
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402

REVIEW_DIR = Path.home() / '.cache/manifold/detector-lab/review'
GOAL = Path.home() / '.cache/manifold/kick-goal-2026-10-10'
LABEL_FILES = {'kick': ('kick_labels.json', 'kick_labels_project.json'), 'snare': ('snare_labels.json',)}
FAMILIES = ('kick', 'snare')
OUT_SR = 48000
BEATS_PER_BAR = 4                 # every set is read as 4/4; a set whose time signature says otherwise is reported
GAP_BARS = 2
LABEL_BEATS = 0.0625              # a label is a point; this is the shortest clip that still shows and clicks
RAMP_STEP_BEATS = 0.25            # a tempo ramp becomes steps this long, each exact in seconds at both ends
FALLBACK_BPM = 120.0
CHECK_MIN_BARS = 8                # a shorter disagreement is a fill or a break, not a section worth a jump marker
CHECK_MERGE_GAP_BARS = 1          # one silent or unsure bar inside a stretch does not split it
TS_FOUR_FOUR = 201                # Live's encoding of 4/4 in the main track's time signature
MOVE_EPS_BEATS = 1e-6
ATTACK_WINDOW_S = 0.020
MASTER_NOT = re.compile(r'PREMASTER|WIP')       # a version tag that names a premaster or a work in progress
REVISION = re.compile(r'(?<![A-Z])[TV](\d+)(?![0-9])')   # the T or V number of a version tag ('T2 MASTER', 'MSTR V3')
LOSSLESS = ('.wav', '.aif', '.aiff', '.flac')
REMASTER = re.compile(r'(?<![a-z])re-?master', re.I)   # the catalogue files these as full mixes
SIDECAR_VERSION = 2
LANES = (  # (name, type, colour, family, kind)
    ('Mix', 'audio', '#5B8DEF', None, 'mix'),
    ('Drums', 'audio', '#8A94A6', None, 'drums'),
    ('Kick labels', 'trigger', '#E5533D', 'kick', 'label'),
    ('Snare labels', 'trigger', '#F2B33D', 'snare', 'label'),
    ('Kick fires', 'trigger', '#9C2F22', 'kick', 'fire'),
    ('Snare fires', 'trigger', '#A97A1B', 'snare', 'fire'),
    ('Kick unsure', 'trigger', '#8E5BA8', 'kick', 'unsure'),
    ('Snare unsure', 'trigger', '#4F8A8B', 'snare', 'unsure'),
)
TRIGGER_OWNER = 'Drums'           # trigger lanes live under an owner layer; Drums keeps the lane order as listed
LANE_OF = {(fam, kind): name for name, _, _, fam, kind in LANES if fam}
LANE_INFO = {name: (fam, kind) for name, _, _, fam, kind in LANES}


# ---- tempo maps ----------------------------------------------------------------------------------------------------

class AbletonMap:
    """An Ableton arrangement tempo map: BPM linear in beats between points (a step is two points at one beat).
    Seconds count from beat 0; before the first point and after the last the tempo holds."""

    def __init__(self, points):
        pts = sorted(((float(b), float(v)) for b, v in points), key=lambda p: p[0])
        if not pts or any(v <= 0 for _, v in pts):
            raise ValueError('a tempo map needs points with positive BPM')
        self.pts = pts
        self.t = [0.0]
        for (b0, v0), (b1, v1) in zip(pts, pts[1:]):
            self.t.append(self.t[-1] + self._span(b0, v0, b1, v1, b1 - b0))
        zero = self.seconds(0.0)   # seconds count from beat 0
        self.t = [x - zero for x in self.t]

    @staticmethod
    def _span(b0, v0, b1, v1, x):
        """Seconds from b0 to b0 + x inside the segment (b0, v0) -> (b1, v1)."""
        if b1 <= b0 or v1 == v0:
            return 60 * x / v0
        k = (v1 - v0) / (b1 - b0)
        return 60 / k * math.log1p(k * x / v0)

    def _seg(self, beat):
        return max(0, bisect.bisect_right([p[0] for p in self.pts], beat) - 1)

    def seconds(self, beat):
        beat = float(beat)
        if beat < self.pts[0][0]:
            return self.t[0] - (self.pts[0][0] - beat) * 60 / self.pts[0][1]
        i = self._seg(beat)
        b0, v0 = self.pts[i]
        b1, v1 = self.pts[i + 1] if i + 1 < len(self.pts) else (math.inf, v0)
        return self.t[i] + self._span(b0, v0, b1, v1, beat - b0)

    def beat(self, sec):
        sec = float(sec)
        if sec < self.t[0]:
            return self.pts[0][0] - (self.t[0] - sec) * self.pts[0][1] / 60
        i = max(0, bisect.bisect_right(self.t, sec) - 1)
        b0, v0 = self.pts[i]
        b1, v1 = self.pts[i + 1] if i + 1 < len(self.pts) else (math.inf, v0)
        d = sec - self.t[i]
        if b1 <= b0 or v1 == v0 or b1 == math.inf:
            return b0 + d * v0 / 60
        k = (v1 - v0) / (b1 - b0)
        return b0 + v0 * math.expm1(k * d / 60) / k


class StepMap:
    """MANIFOLD's tempo map: piecewise-constant BPM from each point's beat; seconds count from beat 0."""

    def __init__(self, points):
        pts = sorted(((float(b), float(v)) for b, v in points), key=lambda p: p[0])
        if not pts or pts[0][0] != 0.0:
            raise ValueError('a project tempo map starts at beat 0')
        self.pts = pts
        self.beats = [b for b, _ in pts]
        self.t = [0.0]
        for (b0, v0), (b1, _) in zip(pts, pts[1:]):
            self.t.append(self.t[-1] + 60 * (b1 - b0) / v0)

    def seconds(self, beat):
        i = max(0, bisect.bisect_right(self.beats, beat) - 1)
        return self.t[i] + 60 * (beat - self.pts[i][0]) / self.pts[i][1]

    def beat(self, sec):
        i = max(0, bisect.bisect_right(self.t, sec) - 1)
        return self.pts[i][0] + (sec - self.t[i]) * self.pts[i][1] / 60


def window_steps(amap, b_from, b_to, step=RAMP_STEP_BEATS):
    """[(song beat, bpm)] steps covering [b_from, b_to) whose seconds equal the Ableton map's at every step edge, and
    the worst gap (s) between the two maps inside a step. A constant stretch is one step at its own BPM (exact); a
    ramp is cut at every multiple of `step`, each piece at the BPM that spends the ramp's own time on it."""
    edges = {b_from, b_to}
    edges |= {b for b, _ in amap.pts if b_from < b < b_to}
    for (b0, v0), (b1, v1) in zip(amap.pts, amap.pts[1:]):
        if v0 != v1 and b1 > b0:
            lo, hi = max(b0, b_from), min(b1, b_to)
            k = math.floor(lo / step) + 1
            while k * step < hi:
                edges.add(k * step)
                k += 1
    edges = sorted(edges)
    out, worst = [], 0.0
    for p, q in zip(edges, edges[1:]):
        if q - p < 1e-12:
            continue
        sp, sq = amap.seconds(p), amap.seconds(q)
        i = amap._seg(p)
        b0, v0 = amap.pts[i]
        b1, v1 = amap.pts[i + 1] if i + 1 < len(amap.pts) else (math.inf, v0)
        constant = v0 == v1 or b1 == math.inf or b1 <= b0
        bpm = v0 if constant else 60 * (q - p) / (sq - sp)
        if not constant:
            xs = np.linspace(p, q, 33)[1:-1]
            worst = max(worst, max(abs(amap.seconds(x) - (sp + (x - p) * 60 / bpm)) for x in xs))
        if out and abs(out[-1][1] - bpm) < 1e-12:
            continue
        out.append((p, bpm))
    return out, worst


# ---- songs ---------------------------------------------------------------------------------------------------------

@dataclass
class AudioPlan:
    """What a song's window plays. Audio seconds = labelled-file seconds + shift_s; the labels, the sidecar and the
    overlays stay in labelled-file seconds."""
    source: str = 'labelled'      # 'master' | 'labelled'
    path: str | None = None       # the master file (source 'master')
    sha256: str | None = None
    shift_s: float = 0.0
    why: str = ''                 # why this source, one line for the start marker and the report
    master: dict | None = None    # the latest master found, with its proof status
    duration_s: float | None = None   # the master's length at the song's rate (its proof row), for --dry-run
    channels: int | None = None   # set by write_audio
    mono_why: str | None = None   # set by write_audio when a song that should be stereo plays mono

    @property
    def label(self):
        """The start marker's note: the why without its detail ('' for a plan with no why)."""
        if self.source == 'labelled' and not self.why:
            return ''
        out = f"master {self.master['version']}" if self.source == 'master' else f"lab mix: {self.why.split(' (')[0]}"
        return out + (f'; mono: {self.mono_why}' if self.mono_why else '')


@dataclass
class SongPlan:
    id: str
    duration_s: float             # of the audio the window plays
    amap: AbletonMap
    offset_s: float               # audio seconds = arrangement seconds + offset_s
    tempo_source: str             # 'ableton' | 'fitted' | 'no grid'
    als: str | None = None
    locators: list = field(default_factory=list)   # [(arrangement beat, name)]
    notes: list = field(default_factory=list)
    sample_rate: int = OUT_SR
    audio: AudioPlan = field(default_factory=AudioPlan)

    @property
    def shift_s(self):
        return self.audio.shift_s

    @property
    def start_beat(self):
        """Arrangement beat at audio time 0."""
        return self.amap.beat(-self.offset_s)

    @property
    def a0_beat(self):
        """First beat of the bar the mix starts in."""
        return BEATS_PER_BAR * math.floor(self.start_beat / BEATS_PER_BAR + 1e-9)

    @property
    def end_beat(self):
        return self.amap.beat(self.duration_s - self.offset_s)

    @property
    def mix_bars(self):
        return max(1, math.ceil((self.end_beat - self.a0_beat) / BEATS_PER_BAR - 1e-9))

    @property
    def span_beats(self):
        return BEATS_PER_BAR * (self.mix_bars + GAP_BARS)


@dataclass
class Window:
    song: SongPlan
    start_beat: float             # project beat of Ableton bar A0
    steps: list                   # [(project beat, bpm)]
    ramp_error_s: float
    mix_proj_s: float = 0.0       # project second of audio time 0

    @property
    def end_beat(self):
        return self.start_beat + self.song.span_beats


def layout(plans):
    """(windows, project StepMap): each song's window starts on a bar, its tempo steps follow its own map, and the
    audio time t sits at project second mix_proj_s + t."""
    wins, beat = [], 0.0
    for s in plans:
        steps, err = window_steps(s.amap, s.a0_beat, s.a0_beat + s.span_beats)
        wins.append(Window(s, beat, [(beat + b - s.a0_beat, v) for b, v in steps], err))
        beat += s.span_beats
    pmap = StepMap([p for w in wins for p in w.steps])
    for w in wins:
        w.mix_proj_s = pmap.seconds(w.start_beat) - w.song.offset_s - w.song.amap.seconds(w.song.a0_beat)
    return wins, pmap


def to_beat(pmap, w, t):
    """Project beat of audio time t in window w."""
    return pmap.beat(w.mix_proj_s + t)


CLIP_GAP_BEATS = 1e-9             # project_tool's JSON float parse can land an ulp off; a clip cut at the next start
                                  # stops this far short of it so the overlap test never sees a rounding overlap


def fit_before(start, dur, nxt):
    """dur, cut so the clip ends CLIP_GAP_BEATS before nxt when it would reach it."""
    return min(dur, nxt - start - CLIP_GAP_BEATS) if start + dur > nxt - CLIP_GAP_BEATS else dur


def point_clips(beats, length=LABEL_BEATS):
    """[(start, duration, index)] for points, overlap-free: each clip is cut at the next start; a point on top of
    the previous one is dropped (its index is left out)."""
    order = sorted(range(len(beats)), key=lambda i: beats[i])
    out = []
    for n, i in enumerate(order):
        b = beats[i]
        if out and b - out[-1][0] < 1e-9:
            continue
        nxt = next((beats[j] for j in order[n + 1:] if beats[j] - b >= 1e-9), math.inf)
        out.append((b, fit_before(b, min(length, nxt - b), nxt), i))
    return out


def span_clips(spans, merge, min_len=LABEL_BEATS):
    """[(start, duration, [indices])] for (start, end) beat spans, overlap-free. merge=True joins overlapping spans
    (unsure); merge=False cuts each at the next start (fires)."""
    order = sorted(range(len(spans)), key=lambda i: spans[i][0])
    out = []
    for n, i in enumerate(order):
        a, b = spans[i]
        b = max(b, a + min_len)
        if merge:
            if out and a <= out[-1][0] + out[-1][1]:
                s0, d0, idx = out[-1]
                out[-1] = (s0, max(d0, b - s0), idx + [i])
            else:
                out.append((a, b - a, [i]))
            continue
        if out and a - out[-1][0] < 1e-9:
            continue
        nxt = next((spans[j][0] for j in order[n + 1:] if spans[j][0] - a >= 1e-9), math.inf)
        out.append((a, fit_before(a, min(b, nxt) - a, nxt), [i]))
    return out


def merge_spans(spans):
    out = []
    for a, b in sorted(spans):
        if out and a <= out[-1][1]:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return out


def check_stretches(n_bars, fires, labels, unsure_bars):
    """[(first bar, bars, 'fires'|'labels')]: stretches of at least CHECK_MIN_BARS bars where one lane has hits and
    the other none. Fires with no labels does not count inside unsure bars. A neutral bar (neither lane, or unsure)
    of at most CHECK_MERGE_GAP_BARS inside a stretch joins it."""
    state = []
    for j in range(n_bars):
        f, lab = j in fires, j in labels
        state.append('fires' if f and not lab and j not in unsure_bars else 'labels' if lab and not f else
                     'agree' if f and lab else None)
    out = []
    for kind in ('fires', 'labels'):
        run = None
        gap = 0
        for j, s in enumerate(state + ['agree']):
            if s == kind:
                run = [j, j] if run is None else [run[0], j]
                gap = 0
            elif run is not None and s is None and gap < CHECK_MERGE_GAP_BARS:
                gap += 1
            else:
                if run is not None and run[1] - run[0] + 1 >= CHECK_MIN_BARS:
                    out.append((run[0], run[1] - run[0] + 1, kind))
                run, gap = None, 0
    return sorted(out)


def check_markers(w, fam, label_beats, fire_beats, unsure_spans):
    if w.song.mix_bars <= 0:
        return []
    bar = lambda b: int(math.floor((b - w.start_beat) / BEATS_PER_BAR))  # noqa: E731
    n = w.song.mix_bars
    fires = {bar(b) for b in fire_beats}
    labels = {bar(b) for b in label_beats}
    unsure = set()
    for a, b in unsure_spans:
        unsure |= set(range(max(0, bar(a)), min(n, bar(b - 1e-9) + 1)))
    out = []
    for j, k, kind in check_stretches(n, fires, labels, unsure):
        what = 'fires, no labels' if kind == 'fires' else 'labels, no fires'
        out.append(dict(beat=w.start_beat + BEATS_PER_BAR * j, name=f'check {fam}: {what} ({k} bars)'))
    return out


# ---- inputs --------------------------------------------------------------------------------------------------------

def load_labels():
    """{family: {song: labels dict}} as the label files hold them (read only)."""
    out = {}
    for fam, files in LABEL_FILES.items():
        out[fam] = {}
        for f in files:
            out[fam].update(json.loads((GOAL / f).read_text()))
    return out


def label_times(lab):
    """[(mix s, subkind)]: positives and the positives inside fast runs (real hits, only unscored)."""
    pts = [(float(t), 'positive') for t in lab.get('positives', [])]
    pts += [(float(t), 'dense_pos') for t in lab.get('dense_pos', [])]
    return sorted(pts)


def load_fires(fires_dir, song, wav=None):
    """[(family, onset s, emit s)] from DIR/<song>.json (JSON list or JSON lines), None when absent, 'stale' when wav
    is given and DIR/<song>.source names another WAV (the fires sit on that WAV's timeline)."""
    if not fires_dir:
        return None
    p = Path(fires_dir) / f'{song}.json'
    if not p.exists():
        return None
    src = p.with_suffix('.source')
    if wav is not None and (not src.exists() or src.read_text() != str(wav)):
        return 'stale'
    text = p.read_text().strip()
    rows = json.loads(text) if text.startswith('[') else [json.loads(x) for x in text.splitlines() if x.strip()]
    return [(r['family'], float(r['onset_s']), float(r['emit_s'])) for r in rows]


def time_signature_events(als):
    """Live's main-track time signature values (201 = 4/4), the manual one first."""
    import gzip
    import xml.etree.ElementTree as ET
    with gzip.open(als, 'rb') as f:
        ls = ET.parse(f).getroot().find('LiveSet')
    main = ls.find('MainTrack') if ls.find('MainTrack') is not None else ls.find('MasterTrack')
    ts = main.find('.//Mixer/TimeSignature')
    if ts is None:
        return []
    vals = [int(ts.find('Manual').get('Value'))]
    tid = ts.find('AutomationTarget').get('Id')
    for env in main.findall('.//AutomationEnvelopes/Envelopes/AutomationEnvelope'):
        if env.find('EnvelopeTarget/PointeeId').get('Value') == tid:
            vals += [int(float(e.get('Value'))) for e in env.findall('Automation/Events/*')]
    return vals


def onset_rise(x, sr, band):
    """The lab's onset envelope: rises of detector_labels.env_db (1 ms grid) in a band."""
    from tools.audio_analysis.eval.detector_labels import env_db
    e = env_db(x, sr, *band)
    return np.maximum(np.diff(e, prepend=e[0]), 0.0)


GRID_BPM_RANGE = (70.0, 180.0)
GRID_NAME_SHARE = 0.9             # a BPM in the song's name wins while its fit is within this share of the best one
GRID_ON_BEAT_S = 0.030            # a kick within this of a fitted beat counts as on the grid
GRID_MIN_ON_BEAT = 0.75           # a grid that misses a quarter of the kicks misleads more than it helps


def phase_coherence(times, bpms):
    """How tightly events sit on a beat grid at each BPM: the mean resultant length of their beat phases."""
    times = np.asarray(times, float)
    out = []
    for i in range(0, len(bpms), 500):
        out.append(np.abs(np.exp(2j * np.pi * np.outer(bpms[i:i + 500] / 60.0, times)).mean(axis=1)))
    return np.concatenate(out)


def fit_grid(kicks, name_bpm=None):
    """(bpm, first downbeat s, note), or (None, None, note) when no steady grid fits, for a song with no Ableton
    tempo. The events are the song's kick labels: for a stem-sum song those are its kick stem's own attacks, for a
    third-party section Peter reviewed them. BPM: the best phase coherence on a 0.01 BPM grid (the name's BPM wins
    within GRID_NAME_SHARE of it). Beat phase: the kicks' mean phase. Downbeat: the beat of four holding the most
    kicks, a guess. An autocorrelation BPM on the mix put under a fifth of the kicks on its grid for
    most of these songs, and Beat This is not installed in the lab env, so neither is used."""
    k = np.sort(np.asarray(kicks, float))
    if len(k) < 8:
        return None, None, 'too few kicks to fit a grid'
    bpms = np.arange(*GRID_BPM_RANGE, 0.01)
    coh = phase_coherence(k, bpms)
    bpm = round(float(bpms[np.argmax(coh)]), 2)
    src = 'fitted to the kicks'
    if name_bpm and GRID_BPM_RANGE[0] <= name_bpm < GRID_BPM_RANGE[1]:
        if phase_coherence(k, np.array([name_bpm]))[0] >= GRID_NAME_SHARE * coh.max():
            bpm, src = float(name_bpm), 'from the name'
        else:
            src = f'fitted to the kicks (the name says {name_bpm:g})'
    spb = 60.0 / bpm
    phase = float(np.angle(np.exp(2j * np.pi * k / spb).mean())) / (2 * np.pi) * spb % spb
    off = (k - phase) / spb
    on = np.abs(off - np.round(off)) * spb <= GRID_ON_BEAT_S
    note = f'BPM {bpm:g} {src}, {on.mean():.0%} of kicks on a beat'
    if on.mean() < GRID_MIN_ON_BEAT:
        return None, None, note
    beat = np.round(off[on]).astype(int) % 4
    down = phase + int(np.argmax(np.bincount(beat, minlength=4))) * spb
    return bpm, down, note + ', downbeat guessed'


def latest_master(con, song_id):
    """The song's latest master in the catalogue, or None, as dict(path, paths, version, revision, local, sha256).
    Candidates: the composition's files of role 'master', and full mixes named remaster (no mix-minus, no reference, no premaster or WIP version tag,
    no variants.NOT_BOUNCE name, none shorter than variants.MIN_DUR_S). Byte-identical copies are one master; an
    online-only file with a local file's name is that file's copy. Rank: the version tag's T or V number (none is 0),
    then lossless over lossy, then local over online-only, then the newest date (each master dated by its earliest
    copy: a copy into a release folder is not a new master)."""
    from tools.audio_analysis.catalog.variants import MIN_DUR_S, NOT_BOUNCE
    row = con.execute('SELECT COALESCE(group_override, group_id) FROM songs WHERE id=?', (song_id,)).fetchone()
    if row is None:
        return None
    masters = {p for (p,) in con.execute("SELECT a.path FROM comp_assets ca JOIN assets a USING(path) WHERE ca.comp_id=? "
                                         "AND a.role='master'", (row[0],))}
    rows = [r for r in con.execute(
        "SELECT a.path, a.name, a.version, a.mtime_ns, a.local, a.sha256, a.duration_s FROM comp_assets ca JOIN assets a "
        "USING(path) WHERE ca.comp_id=? AND a.role IN ('master', 'full_mix') AND a.variant IS NULL "
        "AND ca.flags NOT LIKE '%reference%' ORDER BY a.path", (row[0],))
            if (r[0] in masters or REMASTER.search(r[1])) and not MASTER_NOT.search(r[2] or '') and not NOT_BOUNCE.search(r[1]) and not (r[6] is not None and r[6] < MIN_DUR_S)]
    local_sha = {name: sha for _, name, _, _, local, sha, _ in rows if local and sha}
    groups = {}
    for path, name, ver, mtime, local, sha, _ in rows:
        key = sha if local and sha else local_sha.get(name, ('online', name))
        g = groups.setdefault(key, dict(paths=[], version=ver, revision=0, date=mtime, local=False, lossless=False,
                                        sha256=None))
        g['paths'].append(path)
        rev = max([int(n) for n in REVISION.findall(ver or '')] or [0])
        if rev > g['revision']:
            g['revision'], g['version'] = rev, ver
        g['date'] = min(g['date'], mtime)
        g['local'] |= bool(local)
        g['lossless'] |= path.lower().endswith(LOSSLESS)
        if local and sha:
            g['sha256'] = sha
    if not groups:
        return None
    g = max(groups.values(), key=lambda g: (g['revision'], g['lossless'], g['local'], g['date']))
    g['local_paths'] = [p for p in sorted(g['paths'], key=lambda p: (not p.lower().endswith(LOSSLESS), p))
                        if con.execute('SELECT local FROM assets WHERE path=?', (p,)).fetchone()[0]]
    g['path'] = (g['local_paths'] or g['paths'])[0]
    return g


def audio_plan(song, con, proofs):
    """The AudioPlan for a registry song: its latest master when lab/offsets.py --variants proved that master carries
    the song's labels (a 'proven' row of variant_proofs.json for the master's sha256, possibly through 'same audio as'),
    shifted by registry.variant_shift; else the labelled file and why. proofs: registry.variant_proofs()."""
    from tools.audio_analysis.catalog.catalog import is_dataless
    from tools.audio_analysis.lab import registry
    m = latest_master(con, song.id)
    if m is None:
        return AudioPlan(why='no master in the catalogue')
    if not m['local']:
        return AudioPlan(why=f"master online-only ({Path(m['path']).name})", master=m)
    if m['sha256'] in song.source_sha256:
        return AudioPlan(why='the labelled file is the latest master', master=m)
    rows = proofs.get(song.id, ())
    by_id = {r['id']: r for r in rows}
    r = next((r for r in rows if r['sha256'] == m['sha256']), None)
    hops = 0
    while r is not None and r['status'].startswith('excluded (same audio as ') and hops < len(rows):
        r = by_id.get(r['status'][len('excluded (same audio as '):-1])
        hops += 1
    if r is None:
        r = review_proof(song, m, con)
        if 'path' not in r:
            online = r['status'].startswith('master online-only')
            return AudioPlan(why=r['status'] if online else f"master not proven ({r['status']})",
                             master=dict(m, proof=r['status']))
    m = dict(m, proof=r['status'])
    if r['status'].startswith(('excluded (identical audio', 'excluded (a byte-identical copy')) or '(the lab mix)' in r['status']:
        return AudioPlan(why='the labelled file is the latest master (identical audio)', master=m)
    if not r['status'].startswith('proven'):
        return AudioPlan(why=f"master not proven ({r['status']})", master=m)
    # any local copy, or the proven file itself (its decoded audio is the master's: 'same audio as')
    reads = [(p, m['sha256']) for p in m['local_paths']] + [(r['path'], r['sha256'])]
    pick = next(((p, h) for p, h in reads if Path(p).exists() and not is_dataless(p)), None)
    if pick is None:     # evicted since: a review WAV written from it when it was local still plays it
        pick = next(((p, h) for p, h in reads if cached_wav(master_wav_stem(song.id, h))), None)
    if pick is None:
        return AudioPlan(why=f"master online-only ({Path(m['path']).name})", master=m)
    shift = registry.variant_shift(registry.variant_copy(song, r), song)
    return AudioPlan('master', pick[0], pick[1], shift, why=f"master {m['version']} proven, shift {shift * 1e3:+.3f} ms",
                     master=m, duration_s=r.get('duration_s'))


REVIEW_PROOF_VERSION = 1


def review_proof(song, m, con):
    """A variant_proofs.json-shaped row for a latest master that lab/offsets.py --variants never judged, by the same
    code: the catalogue's verdict and lag (catalog.py variants' row when its key is current, else variants.compare
    on the two files), then offsets.prove_variant on the master's own audio. Cached under REVIEW_DIR/proofs and never
    written to variant_proofs.json, so no training copy or run id moves. A row without 'path' is a reason, not a
    proof."""
    from tools.audio_analysis.catalog import variants
    from tools.audio_analysis.catalog.catalog import is_dataless
    from tools.audio_analysis.lab import offsets, registry
    sha = m['sha256']
    key = variants.key_of(song.content_id, sha, REVIEW_PROOF_VERSION)
    cache = REVIEW_DIR / 'proofs' / f'{song.id}_{sha[:12]}.json'
    if cache.exists():
        got = json.loads(cache.read_text())
        if got.get('key') == key:
            return got['row']
    reads = [p for p in m['local_paths'] if Path(p).exists() and not is_dataless(p)]
    if not reads:
        return dict(status=f"master online-only ({Path(m['path']).name})")
    path = reads[0]
    if song.offset_s is None:
        row = dict(status='the song has no export offset or Ableton set')
    else:
        q = ','.join('?' * len(m['paths']))
        v = con.execute(f"SELECT verdict, lag_s, key FROM variants WHERE song_id=? AND ref='lab_mix' AND path IN ({q}) "
                        "ORDER BY path", [song.id, *m['paths']]).fetchone()
        if v and v[2] == variants.key_of(song.content_id, sha):
            verdict, lag = v[0], v[1]
        else:
            cmp = variants.compare(registry.mix(song), variants.read_mono(path, song.sample_rate), song.sample_rate)
            verdict, lag = cmp['verdict'], cmp['lag_s']
        if verdict != 'same_arrangement':
            row = dict(status=f'catalogue verdict {verdict}')
        else:
            als = offsets.source(song.id)[0]
            seen = {hashlib.sha256(np.ascontiguousarray(registry.mix(song)).tobytes()).hexdigest(): f'{song.id} (the lab mix)'}
            _, row = offsets.prove_variant(song, path, sha, lag, als, seen, offsets.fits(song, als))
            row = dict(row, status=f"{row['status']} [review proof, not in variant_proofs.json]")
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_text(json.dumps(dict(key=key, row=row), indent=1, default=str))
    return row


def audio_plans(songs):
    """{song id: AudioPlan} for registry songs."""
    from tools.audio_analysis.catalog.catalog import connect
    from tools.audio_analysis.lab import registry
    con = connect(readonly=True)
    proofs = registry.variant_proofs()
    return {s.id: audio_plan(s, con, proofs) for s in songs}


def plan_songs(only=None):
    """[SongPlan] for the project songs in registry order."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.lab import registry
    from tools.audio_analysis.lab.offsets import source
    labels = load_labels()
    out = []
    reg = registry.load()
    picked = [s for t, s in reg.items() if s.kind == 'project' and s.parent is None and (not only or t in only)]
    audio = audio_plans(picked)
    for s in picked:
        t = s.id
        labs = [labels[f][t] for f in FAMILIES if t in labels[f]]
        dur = max(float(lab['duration_s']) for lab in labs)
        notes = []
        for f in FAMILIES:
            lo = labels[f].get(t, {}).get('offset_s')
            if lo is not None and s.offset_s is not None and abs(lo - s.offset_s) > 1e-6:
                notes.append(f'{f} labels offset {lo} differs from the registry {s.offset_s}')
        if s.offset_s is not None:
            als = str(source(t)[0])
            res = extract(als)
            amap = AbletonMap([(p['beat'], p['bpm']) for p in res['tempo_points']])
            ts = [v for v in time_signature_events(als) if v != TS_FOUR_FOUR]
            if ts:
                notes.append(f'time signature values {sorted(set(ts))} (not 4/4); bars drawn as 4/4')
            p = SongPlan(t, dur, amap, s.offset_s, 'ableton', als, [(loc['beat'], loc['name']) for loc in res['locators']],
                         notes, s.sample_rate)
        else:
            m = re.search(r'_(\d+)bpm$', t)
            bpm, down, note = fit_grid(labels['kick'].get(t, {}).get('positives', []),
                                       float(m.group(1)) if m else None)
            if bpm is None:
                p = SongPlan(t, dur, AbletonMap([(0, FALLBACK_BPM)]), 0.0, 'no grid',
                             notes=notes + [note + '; no grid, drawn at 120 BPM'], sample_rate=s.sample_rate)
            else:
                bar = BEATS_PER_BAR * 60 / bpm
                p = SongPlan(t, dur, AbletonMap([(0, bpm)]), down % bar - bar, 'fitted', notes=notes + [note],
                             sample_rate=s.sample_rate)
        out.append(on_audio(p, audio[t]))
    return out


def on_audio(plan, audio):
    """The plan moved onto the audio it plays. The grid follows the labelled file through offset_s (labelled seconds =
    arrangement seconds + offset_s), and audio seconds = labelled seconds + shift_s, so the audio's offset is
    offset_s + shift_s: the bar lines move with the audio and stay on the same labels."""
    plan.audio = audio
    plan.offset_s += audio.shift_s
    if audio.duration_s:
        plan.duration_s = audio.duration_s
    return plan


# ---- drum stems ----------------------------------------------------------------------------------------------------

DRUM_ROLES = ('drums', 'kick')


def drum_stem_status(song_id):
    """(paths, lag samples, why): the catalogue's drum and kick stems and the lag its unity check proved against the
    lab mix (remix.unity: sample-level, aligned = lag z >= 8 and correlation >= .85); paths None when unproven."""
    from tools.audio_analysis.catalog import catalog
    st = catalog.stems(song_id)
    paths = [p for r in DRUM_ROLES for p in st.get(r, [])]
    if not paths:
        return None, None, 'no drum stems in the catalogue'
    u = Path.home() / '.cache/manifold/detector-lab/remix/unity' / f'{song_id}.json'
    if not u.exists():
        return None, None, 'no unity check'
    r = json.loads(u.read_text())
    from tools.audio_analysis.lab import registry, remix
    if r.get('key') != remix._key(registry.load()[song_id], remix.stem_info(song_id), remix.UNITY_VERSION):
        return None, None, 'unity check is stale (stems or mix changed since); rerun lab/remix.py unity'
    if not r.get('detail', {}).get('aligned'):
        return None, None, f"unity check not aligned (corr {r.get('corr')}, z {r.get('lag_z')})"
    return paths, int(r['lag_samples']), f"unity {r['verdict']}, lag {r['lag_samples']} samples"


# ---- build ---------------------------------------------------------------------------------------------------------

def resample_48k(x, sr):
    if sr == OUT_SR:
        return x
    from math import gcd

    from scipy.signal import resample_poly
    g = gcd(OUT_SR, sr)
    return resample_poly(x, OUT_SR // g, sr // g)


def lane_content(wins, pmap, labels, fires_dir, wavs=None):
    """{lane: [clip dicts]} and markers, all in project beats, plus per-song stats. A clip dict carries `meta` for
    the sidecar (song, family, kind, labelled-file time). Labels and unsure spans move by the song's shift_s onto its
    audio; a label the shift puts outside the audio is left out and counted. Fires are in audio seconds; with wavs
    ({song: WAV path}), fires made from another WAV are skipped as stale."""
    lanes = {name: [] for name, *_ in LANES}
    markers, stats = [], {}
    for w in wins:
        s = w.song
        sh = s.shift_s
        st = stats.setdefault(s.id, {})
        bar_no = int(round(s.a0_beat / BEATS_PER_BAR)) + 1
        start_name = {'ableton': f'{s.id} (bar {bar_no})', 'fitted': f'{s.id} (fitted grid)',
                      'no grid': f'{s.id} (no grid)'}[s.tempo_source]
        markers.append(dict(beat=w.start_beat, name=f'{start_name} [{s.audio.label}]' if s.audio.label else start_name))
        for b, name in s.locators:
            if s.a0_beat <= b < s.a0_beat + s.span_beats:
                markers.append(dict(beat=w.start_beat + b - s.a0_beat, name=name))
        mix_b0, mix_b1 = to_beat(pmap, w, 0.0), to_beat(pmap, w, s.duration_s)
        lanes['Mix'].append(dict(start_beat=mix_b0, duration_beats=mix_b1 - mix_b0, in_point=0.0,
                                 source_duration=s.duration_s, meta=dict(song=s.id, kind='mix', lab_s=-sh)))
        fires = load_fires(fires_dir, s.id, wavs.get(s.id) if wavs else None)
        st['fires'] = 'none' if fires is None else 'stale' if fires == 'stale' else len(fires)
        fires = None if fires == 'stale' else fires
        for fam in FAMILIES:
            lab = labels[fam].get(s.id)
            every = label_times(lab) if lab else []
            pts = [(t, k) for t, k in every if 0.0 <= t + sh <= s.duration_s]
            st[f'{fam}_outside'] = len(every) - len(pts)
            beats = [to_beat(pmap, w, t + sh) for t, _ in pts]
            kept = point_clips(beats)
            for b, d, i in kept:
                lanes[LANE_OF[fam, 'label']].append(dict(start_beat=b, duration_beats=d, meta=dict(
                    song=s.id, family=fam, kind='label', subkind=pts[i][1], lab_s=pts[i][0])))
            st[f'{fam}_labels'] = len(kept)
            st[f'{fam}_label_dupes'] = len(pts) - len(kept)
            uns = merge_spans([(max(0.0, a + sh), min(s.duration_s, b + sh)) for a, b in (lab or {}).get('unscored', [])
                               if b + sh > 0 and a + sh < s.duration_s])
            ub = [(to_beat(pmap, w, a), to_beat(pmap, w, b)) for a, b in uns]
            uclips = span_clips(ub, merge=True)
            for b, d, idx in uclips:
                lanes[LANE_OF[fam, 'unsure']].append(dict(start_beat=b, duration_beats=d, meta=dict(
                    song=s.id, family=fam, kind='unsure', lab_s=uns[idx[0]][0] - sh,
                    lab_end_s=max(uns[i][1] for i in idx) - sh)))
            st[f'{fam}_unsure'] = len(uclips)
            if fires is None:
                st[f'{fam}_checks'] = None
                continue
            fr = sorted((a, max(a, e)) for f, a, e in fires if f == fam and 0 <= a < s.duration_s)
            fb = [(to_beat(pmap, w, a), to_beat(pmap, w, e)) for a, e in fr]
            fclips = span_clips(fb, merge=False)
            for b, d, idx in fclips:
                lanes[LANE_OF[fam, 'fire']].append(dict(start_beat=b, duration_beats=d, meta=dict(
                    song=s.id, family=fam, kind='fire', lab_s=fr[idx[0]][0] - sh, emit_s=fr[idx[0]][1] - sh)))
            st[f'{fam}_fires'] = len(fclips)
            if lab is None:          # the family does not label this song: every fire would read as a missing label
                st[f'{fam}_checks'] = None
                continue
            cm = check_markers(w, fam, [b for b, _, _ in kept], [b for b, _, _ in fclips], ub)
            markers += cm
            st[f'{fam}_checks'] = len(cm)
    return lanes, markers, stats


def assert_overlap_free(lanes):
    for name, clips in lanes.items():
        cs = sorted(clips, key=lambda c: c['start_beat'])
        for a, b in zip(cs, cs[1:]):
            if a['start_beat'] + a['duration_beats'] > b['start_beat'] + 1e-9:
                raise AssertionError(f"{name}: clips overlap at beat {b['start_beat']}")
            if a['duration_beats'] <= 0:
                raise AssertionError(f"{name}: empty clip at beat {a['start_beat']}")


def project_tool_cmd(arg):
    if arg:
        return [arg] if Path(arg).is_file() else shlex.split(arg)   # a bare path may hold spaces
    for p in (ROOT / 'target/debug/project_tool', ROOT / 'target/release/project_tool'):
        if p.exists():
            return [str(p)]
    return ['cargo', 'run', '-q', '--manifest-path', str(ROOT / 'Cargo.toml'), '-p', 'manifold-io', '--bin',
            'project_tool', '--']


def run_tool(cmd, *args, stdin_json=None, tmp=None):
    """Runs project_tool; a JSON payload goes through a temp file (the verbs take paths)."""
    args = list(args)
    if stdin_json is not None:
        tmp.write_text(json.dumps(stdin_json))
        args = [str(tmp) if a == '@json' else a for a in args]
    r = subprocess.run(cmd + args, capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"project_tool {' '.join(args[:3])} failed:\n{r.stderr.strip()}")
    return r.stdout


def parse_ids(text, n):
    text = text.strip()
    try:
        v = json.loads(text)
        ids = [str(x if not isinstance(x, dict) else x.get('clip_id', x.get('id'))) for x in v]
    except json.JSONDecodeError:
        ids = [ln.split()[-1] for ln in text.splitlines() if ln.strip()]
    if len(ids) != n:
        raise SystemExit(f'project_tool printed {len(ids)} clip ids for {n} clips')
    return ids


# ---- review audio: stereo, on the timeline of what each window plays -----------------------------------------------
# The lab's training readers (registry.mix, remix.read_mono) fold to mono and stay as they are; this path is review only.

def read_stereo(path, sr):
    """(n, channels) float64 at sr: remix.read_mono's decoders (scipy's WAV reader, soundfile when it cannot read the
    file) with the channels kept, each resampled alone, so the mean over channels is read_mono's audio."""
    import math

    import soundfile as sf
    from scipy.io import wavfile
    from scipy.signal import resample_poly
    try:
        r, x = wavfile.read(str(path))
        x = x.astype(np.float64) / float(2 ** (x.dtype.itemsize * 8 - 1)) if np.issubdtype(x.dtype, np.integer) \
            else x.astype(np.float64)
    except ValueError:
        x, r = sf.read(str(path), dtype='float64', always_2d=True)
    x = x.reshape(len(x), -1)
    if r != sr:
        g = math.gcd(sr, r)
        x = resample_poly(x, sr // g, r // g, axis=0)
    return x


def shift_channels(x, k, n):
    """remix.shift on every channel of (samples, channels) audio."""
    from tools.audio_analysis.lab import remix
    return np.stack([remix.shift(c, k, n) for c in x.T], axis=1)


def add_channels(total, y):
    """total + y, the longer length and the wider channel count; a mono part goes to every channel, so the mean over
    channels still adds the part once."""
    n, ch = max(len(total), len(y)), max(total.shape[1], y.shape[1])
    out = np.zeros((n, ch))
    out[:len(total)] += total
    out[:len(y)] += y
    return out


def raw_residual_db(ref, x):
    """|ref - x|^2 / |ref|^2 in dB over ref's length (x cut or padded to it)."""
    import math
    y = np.zeros(len(ref))
    y[:min(len(x), len(ref))] = x[:len(ref)]
    pm = float(np.dot(ref, ref))
    return 10 * math.log10(max(float(np.dot(ref - y, ref - y)), 1e-30 * pm) / max(pm, 1e-30))


def stereo_sources(song, con):
    """(paths, None): the original files whose plain sum is the song's labelled audio, or (None, why). A lab-only mono
    cache (.npy) points at its bounce or stem parts through the catalogue's lab_mix."""
    from tools.audio_analysis.catalog.catalog import is_dataless
    src = song.source
    if src['how'] == 'section':
        return None, 'a section of an export'
    if not src['paths'][0].endswith('.npy'):
        paths = [src['paths'][0]]
    else:
        row = con.execute('SELECT lab_mix FROM songs WHERE id=?', (song.id,)).fetchone()
        lm = json.loads(row[0] or 'null') if row else None
        if not lm:
            return None, 'the catalogue does not say what the lab mix is made from'
        paths = lm.get('parts') if lm.get('kind') == 'stem_sum' else [lm['file']] if lm.get('file') else None
        if not paths:
            return None, f"the lab mix is a {lm.get('kind')} with no source file"
    for p in paths:
        if not Path(p).exists():
            return None, f'{Path(p).name} is missing'
        if is_dataless(p):
            return None, 'stems online-only' if len(paths) > 1 else f'{Path(p).name} online-only'
    return paths, None


def labelled_stereo(song, ref, con):
    """(audio (samples, channels) on the labelled file's timeline, None) or (None, why). The original files' plain
    sum, kept only when its mean over channels is the lab's own mono audio at lag 0 (raw residual under
    variants.IDENTICAL_DB): the same audio, so every channel sits on the labels."""
    from tools.audio_analysis.catalog.variants import IDENTICAL_DB
    paths, why = stereo_sources(song, con)
    if paths is None:
        return None, why
    total = np.zeros((0, 1))
    for p in paths:
        total = add_channels(total, read_stereo(p, song.sample_rate))
    res = raw_residual_db(ref, total.mean(axis=1))
    if res > IDENTICAL_DB:
        return None, f'its stereo source does not fold back to the lab audio ({res:.0f} dB)'
    out = np.zeros((len(ref), total.shape[1]))
    out[:min(len(ref), len(total))] = total[:len(ref)]
    return out, None


def write_review_wav(path, x):
    """48 kHz WAV, 24-bit when it fits under full scale (half the disk of float), else float. Written whole then moved
    into place, so a WAV under its final name is always complete: build reuses it (cached_wav)."""
    import soundfile as sf
    tmp = Path(path).with_name(Path(path).stem + '.partial.wav')
    sf.write(str(tmp), x.astype(np.float32), OUT_SR, subtype='PCM_24' if np.abs(x).max() <= 1.0 else 'FLOAT')
    tmp.replace(path)


def cached_wav(stem):
    """(path, channels, seconds) of a review WAV already written under this name (stereo or mono), else None. Its name
    carries what it was made from (the master's sha256, the song's content id, the drum lag), so it is reused without
    reading the source again: Dropbox evicts files minutes after they are read."""
    import soundfile as sf
    for p in (REVIEW_DIR / f'{stem}_stereo.wav', REVIEW_DIR / f'{stem}.wav'):
        if p.exists():
            info = sf.info(str(p))
            return p, info.channels, info.frames / info.samplerate
    return None


def fall_back(plan, why):
    """The plan back on its labelled file: the shift undone, the reason kept."""
    plan.offset_s -= plan.audio.shift_s
    plan.audio = AudioPlan(why=why, master=plan.audio.master)


def master_wav_stem(song_id, sha):
    return f'{song_id}_master_{sha[:12]}'


def write_audio(plans):
    """The audio each plan plays and the proven drum stems as 48 kHz WAVs, stereo where the source is (else mono, the
    reason in plan.audio.mono_why). A WAV already written under its name is reused (cached_wav). Otherwise a master is
    read only when it is local and still hashes to its proof's sha256, else the plan falls back to its labelled file.
    Sets each plan's duration_s to its audio's length. Returns ({song: mix path}, {song: drums path},
    {song: why no drums})."""
    from tools.audio_analysis.catalog.catalog import connect, is_dataless, sha256_file
    from tools.audio_analysis.lab import registry
    songs = registry.load()
    con = connect(readonly=True)
    mixes, drums, skipped = {}, {}, {}
    REVIEW_DIR.mkdir(parents=True, exist_ok=True)
    for plan in plans:
        s = songs[plan.id]
        sr = s.sample_rate
        got = x = None
        if plan.audio.source == 'master':
            a = plan.audio
            stem = master_wav_stem(s.id, a.sha256)
            got = cached_wav(stem)
            if got is None:
                if not Path(a.path).exists() or is_dataless(a.path):
                    fall_back(plan, f'master online-only ({Path(a.path).name})')
                elif sha256_file(a.path) != a.sha256:
                    fall_back(plan, f'master changed since the catalogue hashed it ({Path(a.path).name})')
                else:
                    x = read_stereo(a.path, sr)
        a = plan.audio
        if a.source == 'labelled':
            stem = f'{s.id}_mix_{s.content_id[:12]}'
            got = cached_wav(stem)
            ref = None
            if got is None or got[1] == 1:      # a mono WAV may be an older build's: try stereo again
                ref = registry.mix(s)
                st, a.mono_why = labelled_stereo(s, ref, con)
                if st is not None:
                    got, x = None, st
                elif got is None:
                    x = ref[:, None]
            if ref is not None and abs(len(ref) / sr - plan.duration_s) > 2e-3:
                print(f'{s.id}: lab mix lasts {len(ref) / sr:.3f} s, the labels say {plan.duration_s:.3f} s; '
                      f'the mix length is used')
        if got is None:
            p = REVIEW_DIR / f"{stem}{'_stereo' if x.shape[1] > 1 else ''}.wav"
            write_review_wav(p, resample_48k(x, sr))
            got = (p, x.shape[1], len(x) / sr)
        p, a.channels, plan.duration_s = got
        if a.channels == 1 and a.mono_why is None:
            a.mono_why = 'the source is mono'
        mixes[s.id] = p
        paths, lag, why = drum_stem_status(s.id)
        if paths is None:
            skipped[s.id] = why
            continue
        k = lag + int(round(a.shift_s * sr))     # the unity lag is to the labelled file; the audio is shifted on
        tag = s.content_id[:12] if a.source == 'labelled' else f'master_{a.sha256[:12]}'
        dstem = f'{s.id}_drums_{tag}_{k}'
        dgot = cached_wav(dstem)
        online = any(is_dataless(pp) for pp in paths)
        if dgot is None and online:
            skipped[s.id] = 'drum stems online-only'
            continue
        if dgot is None or (dgot[1] == 1 and not online):     # a mono WAV may be an older build's: try stereo again
            n = int(round(plan.duration_s * sr))
            y = np.zeros((0, 1))
            for pp in paths:
                y = add_channels(y, shift_channels(read_stereo(pp, sr), k, n))
            q = REVIEW_DIR / f"{dstem}{'_stereo' if y.shape[1] > 1 else ''}.wav"
            write_review_wav(q, resample_48k(y, sr))
            dgot = (q,)
        drums[s.id] = dgot[0]
    return mixes, drums, skipped


def online_only_inputs(plans):
    """[(path, what)]: online-only files the review would read if they were local: a latest master that is the only
    reason a song keeps its labelled file, the sources of a stereo labelled file, the drum stems. Read from file flags
    only; nothing is fetched. Peter brings them down himself (catalog.py download PATH --why ...)."""
    from tools.audio_analysis.catalog import catalog
    from tools.audio_analysis.catalog.catalog import connect, is_dataless
    from tools.audio_analysis.lab import registry
    con = connect(readonly=True)
    out = []

    def online(p):
        return Path(p).exists() and is_dataless(p)
    reg = registry.load()
    for plan in plans:
        t, s, a = plan.id, reg[plan.id], plan.audio
        if a.source == 'labelled' and a.why.startswith('master online-only'):
            p = sorted(a.master['paths'], key=lambda p: (not p.lower().endswith(LOSSLESS), p))[0]
            out.append((p, f'{t} latest master'))
        if a.source == 'labelled' and stereo_sources(s, con)[0] is None:
            src = s.source['paths'][0]
            row = con.execute('SELECT lab_mix FROM songs WHERE id=?', (t,)).fetchone()
            lm = json.loads(row[0] or 'null') if row and src.endswith('.npy') else None
            cand = (lm.get('parts') if lm and lm.get('kind') == 'stem_sum' else [lm['file']] if lm and lm.get('file')
                    else [src])
            out += [(p, f'{t} stereo source') for p in cand if p and online(p)]
        st = catalog.stems(t)
        out += [(str(p), f'{t} drum stem') for r in DRUM_ROLES for p in st.get(r, []) if online(p)]
    return out


def build(out, only, fires_dir, dry_run, tool_arg):
    plans = plan_songs(only)
    need = online_only_inputs(plans)
    if need:
        print(f'{len(need)} online-only files would bring songs closer to their masters or to stereo (not fetched):')
        for p, what in need:
            print(f'  {what}: {p}')
    if not dry_run:
        mixes, drums, skipped = write_audio(plans)
    wins, pmap = layout(plans)
    labels = load_labels()
    lanes, markers, stats = lane_content(wins, pmap, labels, fires_dir, None if dry_run else mixes)
    assert_overlap_free(lanes)
    drum_why = {}
    for w in wins:
        paths, _, why = drum_stem_status(w.song.id)
        drum_why[w.song.id] = 'yes' if paths else why
    report(wins, lanes, markers, stats, drum_why)
    if dry_run:
        return
    for c in lanes['Mix']:
        c['path'] = str(mixes[c['meta']['song']])
        if c['meta']['song'] in drums:
            lanes['Drums'].append(dict(c, path=str(drums[c['meta']['song']]),
                                       meta=dict(c['meta'], kind='drums')))
    out = Path(out)
    if out.exists():
        raise SystemExit(f'{out} exists; build never overwrites a project')
    cmd = project_tool_cmd(tool_arg)
    tmp = out.with_suffix('.payload.json')
    try:
        write_project(out, cmd, tmp, wins, pmap, lanes, markers, mixes, drums, skipped)
    except BaseException:
        out.unlink(missing_ok=True)   # a half-built project is never left to be reviewed
        raise
    finally:
        tmp.unlink(missing_ok=True)


def write_project(out, cmd, tmp, wins, pmap, lanes, markers, mixes, drums, skipped):
    run_tool(cmd, 'project', 'new', str(out), '--bpm', str(pmap.pts[0][1]))
    for name, typ, colour, _, _ in LANES:
        parent = ['--parent', TRIGGER_OWNER] if typ == 'trigger' else []
        run_tool(cmd, 'layer', 'add', str(out), '--name', name, '--type', typ, '--color', colour, *parent)
    run_tool(cmd, 'tempo', 'set', str(out), '@json', stdin_json=[dict(beat=b, bpm=v) for b, v in pmap.pts], tmp=tmp)
    clip_meta = {}
    for name, *_ in LANES:
        clips = sorted(lanes[name], key=lambda c: c['start_beat'])
        if not clips:
            continue
        payload = [{k: v for k, v in c.items() if k != 'meta'} for c in clips]
        ids = parse_ids(run_tool(cmd, 'clip', 'add-bulk', str(out), '--layer', name, '--json', '@json',
                                 stdin_json=payload, tmp=tmp), len(clips))
        for i, c in zip(ids, clips):
            clip_meta[i] = dict(c['meta'], lane=name, start_beat=c['start_beat'])
    run_tool(cmd, 'marker', 'add-bulk', str(out), '--json', '@json', stdin_json=markers, tmp=tmp)
    # The tool's own seconds for each mix clip are the read-back anchor; they must agree with this layout.
    tool_mix = {clip_meta[c['clip_id']]['song']: c['start_s'] for c in tool_json(cmd, 'clip', 'list', str(out), '--json')
                if clip_meta.get(c['clip_id'], {}).get('kind') == 'mix'}
    for w in wins:
        if abs(tool_mix[w.song.id] - w.mix_proj_s) > 1e-3:
            raise SystemExit(f'{w.song.id}: project_tool puts the mix at {tool_mix[w.song.id]:.6f} s, the layout at '
                             f'{w.mix_proj_s:.6f} s; the tempo maps disagree')
        w.mix_proj_s = tool_mix[w.song.id]
    sidecar = dict(version=SIDECAR_VERSION, project=str(out), project_sha256=hashlib.sha256(out.read_bytes()).hexdigest(),
                   tempo=[[b, v] for b, v in pmap.pts], drums_skipped=skipped, clips=clip_meta,
                   songs=[dict(id=w.song.id, window_start_beat=w.start_beat, window_end_beat=w.end_beat,
                               window_start_s=pmap.seconds(w.start_beat), window_end_s=pmap.seconds(w.end_beat),
                               mix_proj_s=w.mix_proj_s, duration_s=w.song.duration_s, tempo_source=w.song.tempo_source,
                               als=w.song.als, offset_s=w.song.offset_s, a0_bar=int(w.song.a0_beat // BEATS_PER_BAR) + 1,
                               ramp_error_s=w.ramp_error_s, mix_wav=str(mixes[w.song.id]),
                               drums_wav=str(drums[w.song.id]) if w.song.id in drums else None, notes=w.song.notes,
                               shift_s=w.song.shift_s, audio=audio_record(w.song.audio))
                          for w in wins])
    sidecar_path(out).write_text(json.dumps(sidecar, indent=1))
    print(f'wrote {out} and {sidecar_path(out)}')
    for t, why in skipped.items():
        print(f'  no drums lane for {t}: {why}')


def audio_record(a):
    """The sidecar's record of what a window plays: read-back needs source and shift_s; the rest is for Peter."""
    m = a.master or {}
    return dict(source=a.source, path=a.path, sha256=a.sha256, shift_s=a.shift_s, why=a.why, channels=a.channels,
                mono_why=a.mono_why, latest_master=dict(path=m.get('path'), version=m.get('version'), local=m.get('local'),
                                                        sha256=m.get('sha256'), proof=m.get('proof')) if m else None)


def song_list(only=None):
    """Plans with id, duration s, offset_s and audio for the project songs in registry order, from the label files
    and the catalogue (no set reads): what write_audio needs."""
    from types import SimpleNamespace

    from tools.audio_analysis.lab import registry
    labels = load_labels()
    picked = [s for t, s in registry.load().items() if s.kind == 'project' and s.parent is None and (not only or t in only)]
    audio = audio_plans(picked)
    out = []
    for s in picked:
        dur = max(float(labels[f][s.id]['duration_s']) for f in FAMILIES if s.id in labels[f])
        out.append(on_audio(SimpleNamespace(id=s.id, duration_s=dur, offset_s=0.0), audio[s.id]))
    return out


def fires_cmd(arg):
    if arg:
        return [arg] if Path(arg).is_file() else shlex.split(arg)   # a bare path may hold spaces
    p = ROOT / 'target/release/examples/detector_fires'
    if p.exists():
        return [str(p)]
    return ['cargo', 'run', '-q', '--release', '--manifest-path', str(ROOT / 'Cargo.toml'), '-p', 'manifold-audio',
            '--example', 'detector_fires', '--']


def make_fires(fires_dir, only, cmd_arg, force):
    """Runs the shipped detectors (manifold-audio's detector_fires example) on each song's mix WAV, one song at a
    time, into DIR/<song>.json (JSON lines). A song is skipped when its file was made from the same WAV."""
    d = Path(fires_dir)
    d.mkdir(parents=True, exist_ok=True)
    mixes, _, _ = write_audio(song_list(only))
    cmd = fires_cmd(cmd_arg)
    for t, wav in mixes.items():
        out, src = d / f'{t}.json', d / f'{t}.source'
        if not force and out.exists() and src.exists() and src.read_text() == str(wav):
            continue
        r = subprocess.run(cmd + [str(wav), '--skip-check'], capture_output=True, text=True)
        if r.returncode:
            raise SystemExit(f'detector_fires failed on {t}:\n{r.stderr.strip()[-2000:]}')
        rows = [json.loads(x) for x in r.stdout.splitlines() if x.strip()]
        bad = [x for x in rows if set(x) != {'family', 'onset_s', 'emit_s'}]
        if bad:
            raise SystemExit(f'{t}: unexpected fire row {bad[0]}')
        tmp = out.with_suffix('.tmp')
        tmp.write_text(''.join(json.dumps(x) + '\n' for x in rows))
        tmp.replace(out)
        src.write_text(str(wav))
        n = {f: sum(x['family'] == f for x in rows) for f in sorted({x['family'] for x in rows})}
        print(f'{t:26s} ' + ', '.join(f'{f} {k}' for f, k in n.items()), flush=True)


def sidecar_path(project):
    p = Path(project)
    return p.with_name(p.stem + '.review.json')


def report(wins, lanes, markers, stats, drum_why):
    total = {k: len(v) for k, v in lanes.items()}
    worst = max((w.ramp_error_s for w in wins), default=0.0)
    print(f'{len(wins)} songs, {wins[-1].end_beat if wins else 0:.0f} beats; worst ramp step error {worst * 1e3:.4f} ms')
    for w in wins:
        s, st = w.song, stats[w.song.id]
        ramps = sum(1 for (b0, v0), (b1, v1) in zip(s.amap.pts, s.amap.pts[1:]) if v0 != v1 and b1 > b0
                    and b1 > s.a0_beat and b0 < s.a0_beat + s.span_beats)
        steps = len(w.steps)
        bpms = sorted({round(v, 3) for _, v in w.steps})
        chk = '/'.join('-' if st.get(f'{f}_checks') is None else str(st[f'{f}_checks']) for f in FAMILIES)
        flag = ' MANY CHECKS' if any((st.get(f'{f}_checks') or 0) > 6 for f in FAMILIES) else ''
        print(f'{s.id:26s} {s.tempo_source:8s} bar {int(s.a0_beat // BEATS_PER_BAR) + 1:4d} bars {s.mix_bars:4d} '
              f'bpm {bpms[0] if len(bpms) == 1 else f"{bpms[0]}..{bpms[-1]} ({steps} steps, {ramps} ramps)"} '
              f'kick {st.get("kick_labels", 0)}/{st.get("kick_unsure", 0)} snare {st.get("snare_labels", 0)}/'
              f'{st.get("snare_unsure", 0)} checks k/s {chk}{flag} drums {drum_why.get(s.id, "?")}'
              + (f' fires {st["fires"]}' if st.get('fires') == 'stale' else '')
              + ''.join(f' {f} outside the audio {st[f"{f}_outside"]}' for f in FAMILIES if st.get(f'{f}_outside'))
              + f'  audio: {s.audio.why}' + (f' ({s.audio.channels} ch)' if s.audio.channels else '')
              + (f'; mono: {s.audio.mono_why}' if s.audio.mono_why else '')
              + (f'  [{"; ".join(s.notes)}]' if s.notes else ''))
    by = {}
    for w in wins:
        by.setdefault(w.song.audio.source if w.song.audio.source == 'master' else w.song.audio.label, []).append(w.song.id)
    for k, ids in sorted(by.items(), key=lambda kv: -len(kv[1])):
        print(f'audio {k}: {len(ids)} ({", ".join(ids)})')
    ch = [w.song.audio.channels for w in wins if w.song.audio.channels]
    if ch:
        print(f'channels: {sum(c > 1 for c in ch)} stereo, {sum(c == 1 for c in ch)} mono')
    print('lanes: ' + ', '.join(f'{k} {v}' for k, v in total.items()))
    print(f'markers: {len(markers)} ({sum(m["name"].startswith("check ") for m in markers)} check)')


# ---- read ----------------------------------------------------------------------------------------------------------

def tool_json(cmd, *args):
    return json.loads(run_tool(cmd, *args))


def attack_time(x, sr, t, band):
    """The steepest rise of the lab onset envelope within +-ATTACK_WINDOW_S of t (s), or None near the ends."""
    a = int((t - 0.2) * sr)
    b = int((t + 0.1) * sr)
    if a < 0 or b > len(x):
        return None
    r = onset_rise(x[a:b], sr, band)
    lo, hi = int(round((0.2 - ATTACK_WINDOW_S) * 1e3)), int(round((0.2 + ATTACK_WINDOW_S) * 1e3)) + 1
    return a / sr + (lo + int(np.argmax(r[lo:hi]))) * 1e-3


def family_bands():
    from tools.audio_analysis.lab import recipe
    recipe.apply()
    from tools.audio_analysis.eval.kick_labels import project_family
    from tools.audio_analysis.eval.snare_labels import FAMILY as SNARE
    return {'kick': project_family().band, 'snare': SNARE.band}


def song_at(songs, sec):
    for s in songs:
        if s['window_start_s'] <= sec < s['window_end_s']:
            return s
    return None


def diff(sidecar, orig_clips, edited_clips, edited_markers):
    """The edits, by clip id, as plain data: {family: {song: {not_hits, moved, new_hits, unsure_resolved,
    lane_moves}}}, other edits, reviewed songs. Every time is in the labelled file's seconds: an edited clip's is its
    project second minus its song's mix_proj_s (the window it now sits in) and shift_s (0 in a version 1 sidecar)."""
    songs = sidecar['songs']
    meta = sidecar['clips']
    orig = {c['clip_id']: c for c in orig_clips}
    edit = {c['clip_id']: c for c in edited_clips}
    fam_out = {f: {} for f in FAMILIES}
    other = {}

    def bucket(fam, song):
        return fam_out[fam].setdefault(song, dict(not_hits=[], moved=[], new_hits=[], unsure_resolved=[], lane_moves=[]))

    def lab_time(c):
        s = song_at(songs, c['start_s'])
        return (s['id'], c['start_s'] - s['mix_proj_s'] - s.get('shift_s', 0.0)) if s else (None, None)

    for cid, m in meta.items():
        if m['kind'] != 'label':
            if cid not in edit:
                other[f"deleted {m['kind']} clips"] = other.get(f"deleted {m['kind']} clips", 0) + 1
            continue
        c = edit.get(cid)
        if c is None:
            bucket(m['family'], m['song'])['not_hits'].append(m['lab_s'])
            continue
        fam_now, kind_now = LANE_INFO.get(c['layer'], (None, None))
        song_now, t_now = lab_time(c)
        if kind_now != 'label':
            other['label clips moved off the label lanes'] = other.get('label clips moved off the label lanes', 0) + 1
            bucket(m['family'], m['song'])['not_hits'].append(m['lab_s'])
            continue
        if fam_now != m['family']:
            bucket(m['family'], m['song'])['not_hits'].append(m['lab_s'])
            bucket(fam_now, song_now)['new_hits'].append(dict(grid_s=t_now, from_family=m['family']))
            bucket(m['family'], m['song'])['lane_moves'].append(dict(lab_s=m['lab_s'], to=fam_now))
            continue
        if abs(c['start_beat'] - orig[cid]['start_beat']) > MOVE_EPS_BEATS:
            if song_now != m['song']:
                bucket(m['family'], m['song'])['not_hits'].append(m['lab_s'])
                bucket(fam_now, song_now)['new_hits'].append(dict(grid_s=t_now))
            else:
                bucket(fam_now, song_now)['moved'].append(dict(from_s=m['lab_s'], grid_s=t_now))
    for cid, c in edit.items():
        if cid in meta or cid in orig:
            continue
        fam_now, kind_now = LANE_INFO.get(c['layer'], (None, None))
        if kind_now != 'label':
            key = f"new clips on {c['layer']}"
            other[key] = other.get(key, 0) + 1
            continue
        song_now, t_now = lab_time(c)
        if song_now is None:
            other['new label clips outside every song'] = other.get('new label clips outside every song', 0) + 1
            continue
        bucket(fam_now, song_now)['new_hits'].append(dict(grid_s=t_now))
    unsure = {}
    for cid, m in meta.items():
        if m['kind'] == 'unsure':
            unsure.setdefault((m['family'], m['song']), []).append((m['lab_s'], m['lab_end_s']))
    for fam, by in fam_out.items():
        for song, b in by.items():
            hits = [h['grid_s'] for h in b['new_hits']] + [h['grid_s'] for h in b['moved']]
            for a, e in sorted(unsure.get((fam, song), [])):
                got = [t for t in hits if a <= t <= e]
                if got:
                    b['unsure_resolved'].append(dict(span=[a, e], hits=sorted(got)))
    win = {s['id']: (s['window_start_beat'], s['window_end_beat']) for s in songs}
    reviewed = sorted({s for m in edited_markers if m['name'].strip().lower() == 'reviewed'
                       for s, (a, b) in win.items() if a <= m['beat'] < b})
    return fam_out, other, reviewed


def refine(fam_out, sidecar, bands):
    """Adds attack_s to every moved and new hit, from the song's WAV (in its seconds, so through shift_s)."""
    import soundfile as sf
    wav = {s['id']: s['mix_wav'] for s in sidecar['songs']}
    shift = {s['id']: s.get('shift_s', 0.0) for s in sidecar['songs']}
    cache = {}
    for fam, by in fam_out.items():
        for song, b in by.items():
            for h in b['moved'] + b['new_hits']:
                if song not in cache:
                    x, sr = sf.read(wav[song], dtype='float64')
                    cache = {song: (x if x.ndim == 1 else x.mean(axis=1), sr)}
                x, sr = cache[song]
                at = attack_time(x, sr, h['grid_s'] + shift[song], bands[fam])
                h['attack_s'] = None if at is None else at - shift[song]


def read(original, edited, tool_arg):
    side = json.loads(sidecar_path(original).read_text())
    cmd = project_tool_cmd(tool_arg)
    if tool_json(cmd, 'tempo', 'show', str(edited)) != tool_json(cmd, 'tempo', 'show', str(original)):
        print('WARNING: the edited project has a different tempo map; lab times follow project seconds')
    oc = tool_json(cmd, 'clip', 'list', str(original), '--json')
    ec = tool_json(cmd, 'clip', 'list', str(edited), '--json')
    em = tool_json(cmd, 'marker', 'list', str(edited), '--json')
    by_id = {c['clip_id']: c for c in ec}
    for s in side['songs']:
        mix = [c for c in oc if side['clips'].get(c['clip_id'], {}).get('kind') == 'mix'
               and side['clips'][c['clip_id']]['song'] == s['id']]
        for c in mix:
            if abs(c['start_s'] - s['mix_proj_s']) > 1e-4:
                raise SystemExit(f"{s['id']}: project_tool puts the mix at {c['start_s']} s, the sidecar at "
                                 f"{s['mix_proj_s']} s; the tempo maps disagree")
            e = by_id.get(c['clip_id'])
            if e is None or abs(e['start_beat'] - c['start_beat']) > MOVE_EPS_BEATS:
                print(f"WARNING: {s['id']}: the mix clip was moved or deleted; label times still follow the original")
    fam_out, other, reviewed = diff(side, oc, ec, em)
    refine(fam_out, side, family_bands())
    out_dir = Path(edited).with_name(Path(edited).stem + '.review')
    out_dir.mkdir(exist_ok=True)
    sha = hashlib.sha256(Path(edited).read_bytes()).hexdigest()
    all_songs = [s['id'] for s in side['songs']]
    lines = [f'review of {edited} against {original}', f'reviewed {len(reviewed)} of {len(all_songs)} songs']
    for fam, by in fam_out.items():
        counts = {t: {k: len(v) for k, v in b.items()} for t, b in sorted(by.items())}
        overlay = dict(provenance='peter', family=fam, original=str(original), edited=str(edited), edited_sha256=sha,
                       reviewed=reviewed, unreviewed=[t for t in all_songs if t not in reviewed],
                       songs={t: by[t] for t in sorted(by)}, counts=counts)
        (out_dir / f'{fam}_overlay.json').write_text(json.dumps(overlay, indent=1))
        tot = {k: sum(c[k] for c in counts.values()) for k in ('not_hits', 'moved', 'new_hits', 'unsure_resolved')}
        lines.append(f"{fam}: {tot['not_hits']} not hits, {tot['moved']} moved, {tot['new_hits']} new hits, "
                     f"{tot['unsure_resolved']} unsure spans given a hit")
        for t, c in counts.items():
            lines.append(f"  {t:26s} " + ', '.join(f'{k} {v}' for k, v in c.items() if v))
        moved = [h for b in by.values() for h in b['moved'] + b['new_hits'] if h.get('attack_s') is not None]
        if moved:
            d = np.array([1e3 * (h['attack_s'] - h['grid_s']) for h in moved])
            lines.append(f'  attack minus grid: median {np.median(d):+.1f} ms, 90% within {np.percentile(abs(d), 90):.1f} ms')
    for k, v in other.items():
        lines.append(f'other edits: {k}: {v}')
    lines.append('unreviewed: ' + (', '.join(t for t in all_songs if t not in reviewed) or 'none'))
    (out_dir / 'summary.txt').write_text('\n'.join(lines) + '\n')
    print('\n'.join(lines))
    print(f'overlays in {out_dir} (not applied to any label file)')


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    b = sub.add_parser('build')
    b.add_argument('out')
    b.add_argument('--songs')
    b.add_argument('--fires-dir')
    b.add_argument('--dry-run', action='store_true')
    b.add_argument('--project-tool')
    f = sub.add_parser('fires')
    f.add_argument('fires_dir')
    f.add_argument('--songs')
    f.add_argument('--fires-cmd')
    f.add_argument('--force', action='store_true')
    r = sub.add_parser('read')
    r.add_argument('original')
    r.add_argument('edited')
    r.add_argument('--project-tool')
    a = ap.parse_args(argv)
    if a.cmd == 'build':
        build(a.out, set(a.songs.split(',')) if a.songs else None, a.fires_dir, a.dry_run, a.project_tool)
    elif a.cmd == 'fires':
        make_fires(a.fires_dir, set(a.songs.split(',')) if a.songs else None, a.fires_cmd, a.force)
    else:
        read(a.original, a.edited, a.project_tool)


if __name__ == '__main__':
    main()
