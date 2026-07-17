"""eval/fetch/self_render.py tests — the P3 self-render generator v1 (agent-
composed MIDI with exact ground truth). Covers the MIDI-composition
functions and ground-truth extraction directly (no audio synthesis, no
network) — fast, deterministic checks that the generation loop actually
produces sane, exact ground truth."""

from __future__ import annotations

from eval.fetch.self_render import (
    _make_arp_16th,
    _make_kick_hat_pattern,
    _make_sustained_pad,
    _midi_ground_truth,
)


def test_arp_16th_produces_16_notes_per_bar_with_exact_onsets():
    pm = _make_arp_16th(bpm=128.0, bars=2)
    truth = _midi_ground_truth(pm)
    assert len(truth) == 32  # 16th notes * 2 bars

    sixteenth = 60.0 / 128.0 / 4.0
    for i, note in enumerate(truth):
        assert abs(note["start_sec"] - i * sixteenth) < 1e-6
        assert not note["is_drum"]
        assert note["duration_sec"] > 0.0


def test_sustained_pad_produces_chords_with_no_gaps_or_overlaps():
    pm = _make_sustained_pad(bpm=100.0, chord_bars=2, n_chords=4)
    truth = _midi_ground_truth(pm)
    # 4 chords * 3 notes each (all progressions in self_render.py are triads).
    assert len(truth) == 12
    starts = sorted(set(n["start_sec"] for n in truth))
    assert len(starts) == 4
    # Chords are back-to-back (no overlap): each start is >= previous chord's
    # nominal end.
    bar_sec = 60.0 / 100.0 * 4
    chord_sec = bar_sec * 2
    for i, s in enumerate(starts):
        assert abs(s - i * chord_sec) < 1e-6


def test_kick_hat_pattern_is_four_on_the_floor_plus_straight_eighth_hats():
    pm = _make_kick_hat_pattern(bpm=128.0, bars=1)
    truth = _midi_ground_truth(pm)
    kicks = [n for n in truth if n["pitch"] == 36]
    hats = [n for n in truth if n["pitch"] == 42]
    assert len(kicks) == 4
    assert len(hats) == 8
    assert all(n["is_drum"] for n in truth)

    beat_sec = 60.0 / 128.0
    for i, k in enumerate(sorted(kicks, key=lambda n: n["start_sec"])):
        assert abs(k["start_sec"] - i * beat_sec) < 1e-6
