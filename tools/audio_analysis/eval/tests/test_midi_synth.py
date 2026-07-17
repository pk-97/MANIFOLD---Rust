"""eval/midi_synth.py tests — the shared MIDI -> audio renderer used by both
the MAESTRO fetch (eval/fetch/maestro.py) and the self-render generator
(eval/fetch/self_render.py). No model/network involved, deterministic,
fast."""

from __future__ import annotations

import wave

import numpy as np
import pretty_midi

from eval.midi_synth import synthesize_midi_file


def _make_two_note_midi(tmp_path):
    pm = pretty_midi.PrettyMIDI()
    inst = pretty_midi.Instrument(program=0)
    inst.notes.append(pretty_midi.Note(velocity=100, pitch=60, start=0.0, end=0.5))
    inst.notes.append(pretty_midi.Note(velocity=100, pitch=64, start=0.5, end=1.0))
    pm.instruments.append(inst)
    midi_path = tmp_path / "two_notes.mid"
    pm.write(str(midi_path))
    return midi_path


def test_synthesize_midi_file_writes_nonempty_wav_matching_duration(tmp_path):
    midi_path = _make_two_note_midi(tmp_path)
    wav_path = tmp_path / "out.wav"

    duration = synthesize_midi_file(midi_path, wav_path, sample_rate=44100)

    assert duration > 0.9  # last note ends at 1.0s
    assert wav_path.exists()
    with wave.open(str(wav_path), "rb") as wf:
        assert wf.getnchannels() == 1
        assert wf.getframerate() == 44100
        n_frames = wf.getnframes()
        assert n_frames > 0
        raw = wf.readframes(n_frames)
    pcm = np.frombuffer(raw, dtype=np.int16)
    assert np.max(np.abs(pcm)) > 0  # actually rendered audible content


def test_synthesize_midi_file_never_clips(tmp_path):
    midi_path = _make_two_note_midi(tmp_path)
    wav_path = tmp_path / "out.wav"
    synthesize_midi_file(midi_path, wav_path, sample_rate=44100)
    with wave.open(str(wav_path), "rb") as wf:
        raw = wf.readframes(wf.getnframes())
    pcm = np.frombuffer(raw, dtype=np.int16)
    # Peak-normalized to 0.9 in midi_synth.py — must never hit full-scale.
    assert np.max(np.abs(pcm)) <= int(32767 * 0.9) + 2
