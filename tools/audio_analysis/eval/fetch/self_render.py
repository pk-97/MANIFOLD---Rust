"""Self-render generator v1, per docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §3/P3
dataset table: "agent-composed MIDI -> synth render, on-genre arps/stabs/
pads with perfect truth, generated on demand, grows as gaps appear."

This is deliberately v1 — a minimal proof that the on-demand generation loop
works, not a full generator library. It produces a handful of short,
on-genre (electronic) fixtures with EXACT MIDI-level ground truth (every
note's pitch/onset/duration is known by construction, no annotation or
transcription involved):

  - a 16th-note arpeggio (the exact granularity Peter's own ruling names:
    "a 16th-note arp becomes sixteen clips a bar")
  - a sustained pad/chord progression (duration-event ground truth, the D4
    Chord/sustained-object precedent)
  - a simple 4-on-the-floor kick+hat pattern (drum-truth precedent, for
    ADTOF/onset scoring parity with the manifold_own kick-onset fixtures)
  - an EDM kit pattern (added 2026-07-18, ADTOF bake-off B1) — kick, snare,
    clap, closed hat, and tom, each with a distinct synthesized timbre
    (see synthesize_edm_kit_midi's per-class bursts), used as the known-truth
    fixture for manifold_audio.stage1_dsp_detection's clustering + centroid-
    signature labeling unit tests (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md
    §7.1: "clap/snare/hat/tom via per-onset features"). domain=electronic
    per the bake-off brief's "self-rendered EDM kits" deliverable.

Rendered via eval/midi_synth.py (pretty_midi's additive synth — same
renderer used for the MAESTRO selection). Grows as gaps appear (future
sessions add more generator functions here); this is the seed.

Usage:
    python -m eval.fetch.self_render --out-dir eval/data/self_render
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from eval.paths import DATA_ROOT
from typing import Dict, List

import numpy as np
import pretty_midi

from eval.midi_synth import _write_wav_mono, synthesize_midi_file

LICENSE = "ours (agent-composed MIDI, synth-rendered) — no license restriction"


def _make_arp_16th(bpm: float = 128.0, bars: int = 4, root_midi: int = 57) -> pretty_midi.PrettyMIDI:
    """A 16th-note arpeggio over a minor triad + octave, 16 notes/bar —
    exact onset truth for every note (D3's clip-per-note contract)."""
    pm = pretty_midi.PrettyMIDI(initial_tempo=bpm)
    inst = pretty_midi.Instrument(program=81)  # lead synth
    sixteenth = 60.0 / bpm / 4.0
    pattern = [0, 3, 7, 12]  # minor triad + octave, scale degrees in semitones
    notes: List[pretty_midi.Note] = []
    t = 0.0
    for bar in range(bars):
        for step in range(16):
            pitch = root_midi + pattern[step % len(pattern)]
            start = t
            end = t + sixteenth * 0.9
            notes.append(pretty_midi.Note(velocity=90, pitch=pitch, start=start, end=end))
            t += sixteenth
    inst.notes = notes
    pm.instruments.append(inst)
    return pm


def _make_sustained_pad(bpm: float = 100.0, chord_bars: int = 2, n_chords: int = 4, root_midi: int = 48) -> pretty_midi.PrettyMIDI:
    """Sustained chords, each held for chord_bars bars — duration-event
    ground truth for the Chord/sustained-object precedent (D4)."""
    pm = pretty_midi.PrettyMIDI(initial_tempo=bpm)
    inst = pretty_midi.Instrument(program=89)  # pad
    beats_per_bar = 4
    bar_sec = 60.0 / bpm * beats_per_bar
    chord_sec = bar_sec * chord_bars
    progressions = [[0, 3, 7], [5, 8, 12], [7, 10, 14], [3, 7, 10]]  # i-iv-v-III-ish, minor-key feel
    notes: List[pretty_midi.Note] = []
    for i in range(n_chords):
        start = i * chord_sec
        end = start + chord_sec * 0.98  # tiny gap so chord boundaries are unambiguous onsets
        for interval in progressions[i % len(progressions)]:
            notes.append(pretty_midi.Note(velocity=70, pitch=root_midi + interval, start=start, end=end))
    inst.notes = notes
    pm.instruments.append(inst)
    return pm


def _make_kick_hat_pattern(bpm: float = 128.0, bars: int = 4) -> pretty_midi.PrettyMIDI:
    """4-on-the-floor kick + straight 8th-note hats — drum-truth precedent
    matching the manifold_own kick-onset fixtures' role (grid + drums)."""
    pm = pretty_midi.PrettyMIDI(initial_tempo=bpm)
    inst = pretty_midi.Instrument(program=0, is_drum=True)
    beat_sec = 60.0 / bpm
    eighth_sec = beat_sec / 2.0
    notes: List[pretty_midi.Note] = []
    for bar in range(bars):
        bar_start = bar * beat_sec * 4
        for beat in range(4):
            kick_t = bar_start + beat * beat_sec
            notes.append(pretty_midi.Note(velocity=110, pitch=36, start=kick_t, end=kick_t + 0.05))  # kick = MIDI 36
        for step in range(8):
            hat_t = bar_start + step * eighth_sec
            notes.append(pretty_midi.Note(velocity=70, pitch=42, start=hat_t, end=hat_t + 0.03))  # closed hat = MIDI 42
    inst.notes = notes
    pm.instruments.append(inst)
    return pm


def _make_edm_kit_pattern(bpm: float = 128.0, bars: int = 8) -> pretty_midi.PrettyMIDI:
    """4-on-the-floor kick, snare/clap on 2 & 4 (layered, like a lot of real
    EDM production), closed 16th-note hats, and a tom fill on the last beat
    of every other bar -- GM pitches: kick=36, snare=38, clap=39, closed
    hat=42, low-mid tom=45. Every onset's class is exact by construction
    (the whole point of this fixture: known truth for clustering/labeling
    unit tests, not audio realism).

    Timing is deliberately arranged so DIFFERENT classes never share the
    exact same onset instant: kick sits on integer beat positions; snare is
    offset +50ms off the beat (a "kick under snare" house/EDM layering would
    otherwise put a kick and a snare at the EXACT same instant on beats 1
    and 3 -- physically superimposed in the rendered audio, and no per-onset
    feature/cluster method could ever separate two hits that share one
    onset time, which isn't a meaningful thing to ask this unit test to
    solve); hats are shifted to the OFF sixteenth-grid (+0.5 sixteenth) so a
    hat never lands on the same sample as a kick/snare/clap/tom either.
    Clap is offset +40ms after its layered snare for the same reason --
    close enough to read as "layered production", far enough that
    extract_onset_features' own next-onset window cap gives each a
    mostly-clean analysis window."""
    pm = pretty_midi.PrettyMIDI(initial_tempo=bpm)
    inst = pretty_midi.Instrument(program=0, is_drum=True)
    beat_sec = 60.0 / bpm
    eighth_sec = beat_sec / 2.0
    sixteenth_sec = beat_sec / 4.0
    snare_offset_sec = 0.05
    clap_offset_sec = 0.04
    notes: List[pretty_midi.Note] = []
    for bar in range(bars):
        bar_start = bar * beat_sec * 4
        for beat in range(4):
            kick_t = bar_start + beat * beat_sec
            notes.append(pretty_midi.Note(velocity=110, pitch=36, start=kick_t, end=kick_t + 0.05))
        for beat in (1, 3):  # backbeat: snare, clap layered ~40ms behind
            snare_t = bar_start + beat * beat_sec + snare_offset_sec
            notes.append(pretty_midi.Note(velocity=105, pitch=38, start=snare_t, end=snare_t + 0.05))
            clap_t = snare_t + clap_offset_sec
            notes.append(pretty_midi.Note(velocity=95, pitch=39, start=clap_t, end=clap_t + 0.03))
        for step in range(16):  # off-grid (+0.5 sixteenth): never coincides with kick/snare/clap/tom
            hat_t = bar_start + (step + 0.5) * sixteenth_sec
            notes.append(pretty_midi.Note(velocity=65, pitch=42, start=hat_t, end=hat_t + 0.02))
        if bar % 2 == 1:
            tom_t = bar_start + 3 * beat_sec + eighth_sec
            notes.append(pretty_midi.Note(velocity=100, pitch=45, start=tom_t, end=tom_t + 0.12))
    inst.notes = notes
    pm.instruments.append(inst)
    return pm


def _lowpass_ema(x: np.ndarray, sr: int, fc_hz: float) -> np.ndarray:
    """Single-pole (exponential-moving-average) lowpass -- not remotely a
    sharp filter, just enough to shape a noise burst's spectral center
    without adding a scipy filter-design dependency."""
    alpha = float(np.exp(-2.0 * np.pi * fc_hz / sr))
    y = np.empty_like(x)
    acc = 0.0
    for i in range(len(x)):
        acc = alpha * acc + (1.0 - alpha) * x[i]
        y[i] = acc
    return y


def _snare_burst(sr: int, dur_sec: float = 0.10) -> np.ndarray:
    """Mid-band-limited noise (lowpass(3kHz) - lowpass(200Hz), a crude
    bandpass -- a real snare's "snap" sits in the low-mids, unlike a hat's
    broadband/high noise) + a low tonal thump -- distinct from the hat
    (high-frequency-dominant) and the clap (highpassed) in centroid AND
    band-ratio, not just decay time. Discovered via the self_render EDM-kit
    fixture's own feature diagnostic: an earlier full-broadband-noise
    version of this burst was spectrally indistinguishable from the hat."""
    rng = np.random.default_rng(20260718)
    n = int(round(dur_sec * sr))
    t = np.arange(n) / sr
    noise = rng.standard_normal(n)
    band = _lowpass_ema(noise, sr, 3000.0) - _lowpass_ema(noise, sr, 200.0)
    band = band * np.exp(-t / 0.035)
    tone = np.sin(2 * np.pi * 190.0 * t) * np.exp(-t / 0.02)
    sig = 0.6 * band + 0.5 * tone
    peak = float(np.max(np.abs(sig)))
    if peak > 0:
        sig = sig / peak
    return sig.astype(np.float32)


def _clap_burst(sr: int, dur_sec: float = 0.05) -> np.ndarray:
    """Several fast noise micro-bursts (real claps are multi-transient),
    strongly high-frequency-weighted (highpassed via differencing) and very
    short overall -- distinct from both snare (longer/lower) and hat
    (single clean burst, not multi-transient)."""
    rng = np.random.default_rng(20260719)
    n = int(round(dur_sec * sr))
    audio = np.zeros(n, dtype=np.float64)
    micro_offsets = [0.0, 0.006, 0.013]
    for off in micro_offsets:
        start = int(round(off * sr))
        if start >= n:
            continue
        sub_n = n - start
        t = np.arange(sub_n) / sr
        burst = rng.standard_normal(sub_n) * np.exp(-t / 0.006)
        audio[start:] += burst
    # crude high-pass: first difference emphasizes high frequencies.
    hp = np.diff(audio, prepend=audio[0])
    return hp.astype(np.float32)


def _hat_burst_edm(sr: int, dur_sec: float = 0.03) -> np.ndarray:
    """Single clean noise burst, very fast decay, high-frequency-weighted --
    matches _hat_burst's role but named separately since the EDM kit fixture
    intentionally reuses the same shape as the kick_hat_128bpm fixture's own
    hat (consistency across self-render fixtures)."""
    return _hat_burst(sr, dur_sec=dur_sec)


def _tom_burst(sr: int, dur_sec: float = 0.18) -> np.ndarray:
    """Tonal, low-mid pitched (higher than the kick, still clearly tonal/
    low-flatness), with a LONGER decay than kick/snare/clap/hat -- the decay-
    rate feature is what should separate this from the kick cluster."""
    n = int(round(dur_sec * sr))
    t = np.arange(n) / sr
    env = np.exp(-t / 0.07)
    tone = np.sin(2 * np.pi * 145.0 * t) + 0.3 * np.sin(2 * np.pi * 290.0 * t)
    return (tone * env).astype(np.float32)


_EDM_KIT_BURSTS = {
    36: lambda sr: _kick_burst(sr),
    38: lambda sr: _snare_burst(sr),
    39: lambda sr: _clap_burst(sr),
    42: lambda sr: _hat_burst_edm(sr),
    45: lambda sr: _tom_burst(sr),
}


def synthesize_edm_kit_midi(pm: pretty_midi.PrettyMIDI, sr: int = 44100) -> np.ndarray:
    """Same additive-burst approach as synthesize_drum_midi, generalized to
    the EDM kit's 5 distinct per-class timbres (_EDM_KIT_BURSTS)."""
    end = pm.get_end_time() + 0.5
    total = int(round(end * sr))
    audio = np.zeros(total, dtype=np.float32)
    for inst in pm.instruments:
        for note in inst.notes:
            burst_fn = _EDM_KIT_BURSTS.get(note.pitch)
            if burst_fn is None:
                continue
            burst = burst_fn(sr)
            start = int(round(note.start * sr))
            endi = min(total, start + len(burst))
            audio[start:endi] += burst[: endi - start]
    peak = float(np.max(np.abs(audio)))
    if peak > 0:
        audio = audio / peak * 0.9
    return audio


GENERATORS = {
    "arp_16th_128bpm": lambda: _make_arp_16th(bpm=128.0),
    "sustained_pad_100bpm": lambda: _make_sustained_pad(bpm=100.0),
    "kick_hat_128bpm": lambda: _make_kick_hat_pattern(bpm=128.0),
    "edm_kit_128bpm": lambda: _make_edm_kit_pattern(bpm=128.0),
}

# pretty_midi.Instrument.synthesize() explicitly zeroes drum-channel (is_drum
# =True) instruments ("For drum instruments, returns zeros" — its own
# docstring); the shared synth in eval/midi_synth.py can't render
# kick_hat_128bpm's audible content. Render drum fixtures with a dedicated
# percussive burst synth instead (same shape as
# eval/beat_tracker_alignment.py's _percussive_click — exp-decaying noise for
# the hat; a short low-sine thump for the kick), keeping the MIDI file (and
# its note list) as the exact ground truth either way.
DRUM_FIXTURE_IDS = {"kick_hat_128bpm", "edm_kit_128bpm"}


def _kick_burst(sr: int, dur_sec: float = 0.09) -> np.ndarray:
    n = int(round(dur_sec * sr))
    t = np.arange(n) / sr
    env = np.exp(-t / 0.025)
    tone = np.sin(2 * np.pi * 60.0 * t)  # low thump
    return (tone * env).astype(np.float32)


def _hat_burst(sr: int, dur_sec: float = 0.04) -> np.ndarray:
    rng = np.random.default_rng(20260717)
    n = int(round(dur_sec * sr))
    env = np.exp(-np.arange(n) / (0.008 * sr))
    return (rng.standard_normal(n) * env).astype(np.float32)


def synthesize_drum_midi(pm: pretty_midi.PrettyMIDI, sr: int = 44100) -> np.ndarray:
    end = pm.get_end_time() + 0.5
    total = int(round(end * sr))
    audio = np.zeros(total, dtype=np.float32)
    kick = _kick_burst(sr)
    hat = _hat_burst(sr)
    for inst in pm.instruments:
        for note in inst.notes:
            burst = kick if note.pitch == 36 else hat
            start = int(round(note.start * sr))
            endi = min(total, start + len(burst))
            audio[start:endi] += burst[: endi - start]
    peak = float(np.max(np.abs(audio)))
    if peak > 0:
        audio = audio / peak * 0.9
    return audio


# Per-fixture synth dispatch (drum fixtures only) -- kick_hat_128bpm's 2-class
# burst synth vs edm_kit_128bpm's 5-class one. A drum fixture missing from
# this dict falls through to pretty_midi's own synth, which silently returns
# ZERO audio for is_drum=True instruments (its own docstring: "For drum
# instruments, returns zeros") -- caught 2026-07-18 when edm_kit_128bpm was
# first added without an entry here and produced a silent wav (0 onsets
# detected downstream, not an obviously-wrong-looking failure).
DRUM_SYNTH_FN = {
    "kick_hat_128bpm": synthesize_drum_midi,
    "edm_kit_128bpm": synthesize_edm_kit_midi,
}


def _midi_ground_truth(pm: pretty_midi.PrettyMIDI) -> List[Dict]:
    events = []
    for inst in pm.instruments:
        for note in inst.notes:
            events.append({
                "pitch": note.pitch,
                "start_sec": note.start,
                "end_sec": note.end,
                "duration_sec": note.end - note.start,
                "velocity": note.velocity,
                "is_drum": inst.is_drum,
            })
    events.sort(key=lambda e: e["start_sec"])
    return events


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=DATA_ROOT / "self_render")
    args = parser.parse_args(argv)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    manifest = []
    for name, factory in GENERATORS.items():
        pm = factory()
        midi_path = args.out_dir / f"{name}.mid"
        wav_path = args.out_dir / f"{name}.wav"
        truth_path = args.out_dir / f"{name}_truth.json"
        pm.write(str(midi_path))
        if name in DRUM_FIXTURE_IDS:
            audio = DRUM_SYNTH_FN[name](pm)
            _write_wav_mono(wav_path, audio, 44100)
            duration = pm.get_end_time()
        else:
            duration = synthesize_midi_file(midi_path, wav_path)
        truth = _midi_ground_truth(pm)
        truth_path.write_text(json.dumps(truth, indent=2))
        manifest.append({"id": name, "duration_sec": duration, "n_notes": len(truth), "midi": midi_path.name, "wav": wav_path.name, "truth": truth_path.name})
        print(f"[self_render] {name}: {len(truth)} notes, {duration:.2f}s audio")

    (args.out_dir / "manifest.json").write_text(json.dumps(manifest, indent=2))
    print(f"[self_render] generated {len(manifest)} fixtures -> {args.out_dir}")
    print(f"[self_render] license: {LICENSE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
