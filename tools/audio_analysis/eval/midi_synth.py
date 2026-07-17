"""Shared MIDI -> audio synth, used by both the self-render generator (P3
deliverable #4: agent-composed on-genre arps/pads with perfect MIDI truth)
and the MAESTRO fixture selection (P3 deliverable #2: real MAESTRO MIDI
performances, rendered by us since real recorded piano audio isn't
individually fetchable — see eval/fetch/maestro.py's module docstring for
why). One synth avoids duplicating rendering code across both needs.

Uses pretty_midi's built-in additive-sine `synthesize()` — pure numpy, no
external fluidsynth binary or soundfont required (checked 2026-07-17:
fluidsynth is not installed in this environment; pretty_midi's synthesize()
needs neither). This is NOT a realistic instrument timbre — it is a
deliberately simple, deterministic renderer whose only job is to give
basic_pitch (or any note-level detector) real audio to transcribe against
EXACT MIDI ground truth. Timbre realism is not the point; note-onset/
duration/pitch accuracy of the ground truth is.

Usage as a library:
    from eval.midi_synth import synthesize_midi_file
    synthesize_midi_file(Path("in.mid"), Path("out.wav"), sample_rate=44100)
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pretty_midi


def synthesize_midi_file(midi_path: Path, out_wav_path: Path, sample_rate: int = 44100) -> float:
    """Renders midi_path to a mono wav at out_wav_path. Returns duration in
    seconds. Raises whatever pretty_midi raises on malformed MIDI (best-effort
    callers should catch and log, not let one bad file abort a batch)."""
    pm = pretty_midi.PrettyMIDI(str(midi_path))
    audio = pm.synthesize(fs=sample_rate)
    if audio.size == 0:
        audio = np.zeros(1, dtype=np.float32)
    peak = float(np.max(np.abs(audio)))
    if peak > 0:
        audio = audio / peak * 0.9
    _write_wav_mono(out_wav_path, audio.astype(np.float32), sample_rate)
    return pm.get_end_time()


def _write_wav_mono(path: Path, audio: np.ndarray, sr: int) -> None:
    import wave

    path.parent.mkdir(parents=True, exist_ok=True)
    pcm16 = np.clip(audio * 32767.0, -32768, 32767).astype(np.int16)
    with wave.open(str(path), "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(sr)
        wf.writeframes(pcm16.tobytes())
