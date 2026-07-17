"""eval/full_pack_baseline.py tests — covers the pure-logic pieces that
don't require model inference or fetched datasets: the Harmonix
constant-offset estimator (found necessary 2026-07-17: YouTube-matched
audio commonly carries a multi-second timing offset vs. the annotation
reference) and MIDI ground-truth extraction. The detector-scoring functions
themselves (ADTOF, basic_pitch, madmom, Beat This) are exercised by the real
baseline run (eval/scoreboard/p3_full_pack_baseline.json), not here — same
convention as eval/tests/test_beat_tracker_alignment.py."""

from __future__ import annotations

import numpy as np
import pretty_midi

from eval.full_pack_baseline import _estimate_constant_shift, _midi_truth_notes


def test_estimate_constant_shift_recovers_a_known_offset():
    rng = np.random.default_rng(0)
    truth = sorted(rng.uniform(0, 120, size=200))
    true_shift = 3.7
    # A few predictions are missing/extra to mimic a real detector's output,
    # not a clean permutation of truth.
    pred = [t + true_shift for t in truth[5:-3]]
    pred += [50.0, 51.0, 52.5]  # a few unrelated extra predictions

    shift, votes = _estimate_constant_shift(pred, truth)

    assert abs(shift - true_shift) < 0.05
    assert votes >= 150  # most of the 192 genuinely-shifted truth beats should vote


def test_estimate_constant_shift_returns_zero_on_empty_input():
    shift, votes = _estimate_constant_shift([], [1.0, 2.0])
    assert shift == 0.0
    assert votes == 0
    shift, votes = _estimate_constant_shift([1.0], [])
    assert shift == 0.0
    assert votes == 0


def test_midi_truth_notes_reads_exact_onset_duration_pitch(tmp_path):
    pm = pretty_midi.PrettyMIDI()
    inst = pretty_midi.Instrument(program=0)
    inst.notes.append(pretty_midi.Note(velocity=90, pitch=60, start=0.25, end=0.75))
    inst.notes.append(pretty_midi.Note(velocity=90, pitch=64, start=1.0, end=1.5))
    pm.instruments.append(inst)
    midi_path = tmp_path / "notes.mid"
    pm.write(str(midi_path))

    notes = _midi_truth_notes(midi_path)

    assert len(notes) == 2
    assert notes[0] == {"start_sec": 0.25, "end_sec": 0.75, "pitch": 60}
    assert notes[1] == {"start_sec": 1.0, "end_sec": 1.5, "pitch": 64}
