"""Network-free unit tests for P3's new fetch scripts and shared synth —
covers the pure logic (path parsing, selection algorithms, MIDI generation)
without hitting the network or requiring the fetched data on disk."""

from __future__ import annotations

import pretty_midi
import pytest

from eval.fetch.maestro import select_diverse
from eval.fetch.self_render import (
    _make_arp_16th,
    _make_kick_hat_pattern,
    _make_sustained_pad,
    synthesize_drum_midi,
)
from eval.fetch.slakh2100 import _split_of, _track_of


def test_split_of_parses_redux_archive_layout():
    assert _split_of("slakh2100_flac_redux/test/Track01876/mix.flac") == "test"
    assert _split_of("slakh2100_flac_redux/train/Track00001/MIDI/S00.mid") == "train"
    assert _split_of("slakh2100_flac_redux/omitted/Track01627/metadata.yaml") == "omitted"


def test_split_of_returns_none_for_unrelated_paths():
    assert _split_of("some_other_archive/test/Track00001/mix.flac") is None
    assert _split_of("just_a_file.txt") is None


def test_track_of_extracts_track_id():
    assert _track_of("slakh2100_flac_redux/test/Track01876/mix.flac") == "Track01876"
    assert _track_of("slakh2100_flac_redux/test/Track01876/stems/S00.flac") == "Track01876"
    assert _track_of("slakh2100_flac_redux/test") is None


def test_select_diverse_returns_requested_count_and_stays_within_split():
    rows = [
        {"split": "test", "canonical_composer": f"Composer{i % 5}", "duration": str(100 + i), "midi_filename": f"f{i}.midi"}
        for i in range(50)
    ]
    rows += [{"split": "train", "canonical_composer": "X", "duration": "1", "midi_filename": "train.midi"}]
    picked = select_diverse(rows, split="test", n=10)
    assert len(picked) == 10
    assert all(r["split"] == "test" for r in picked)
    # Deterministic: re-running gives the identical selection.
    picked2 = select_diverse(rows, split="test", n=10)
    assert [r["midi_filename"] for r in picked] == [r["midi_filename"] for r in picked2]


def test_select_diverse_handles_fewer_candidates_than_requested():
    rows = [{"split": "test", "canonical_composer": "A", "duration": "10", "midi_filename": "a.midi"}]
    picked = select_diverse(rows, split="test", n=20)
    assert len(picked) == 1


def test_make_arp_16th_produces_16_notes_per_bar():
    pm = _make_arp_16th(bpm=128.0, bars=2)
    notes = pm.instruments[0].notes
    assert len(notes) == 32  # 16 per bar * 2 bars
    # Sorted onsets, evenly spaced at a 16th note.
    onsets = sorted(n.start for n in notes)
    spb16 = 60.0 / 128.0 / 4.0
    for i in range(1, len(onsets)):
        assert abs((onsets[i] - onsets[i - 1]) - spb16) < 1e-6


def test_make_sustained_pad_produces_chords_with_duration():
    pm = _make_sustained_pad(bpm=100.0, chord_bars=2, n_chords=4)
    notes = pm.instruments[0].notes
    assert len(notes) == 12  # 3-note chords * 4
    for n in notes:
        assert n.end > n.start  # every chord note is a sustained duration event


def test_make_kick_hat_pattern_uses_gm_drum_pitches():
    pm = _make_kick_hat_pattern(bpm=128.0, bars=1)
    pitches = {n.pitch for n in pm.instruments[0].notes}
    assert pitches == {36, 42}  # kick, closed hat
    assert pm.instruments[0].is_drum is True


def test_synthesize_drum_midi_produces_nonzero_audio():
    pm = _make_kick_hat_pattern(bpm=128.0, bars=1)
    audio = synthesize_drum_midi(pm, sr=44100)
    assert audio.size > 0
    assert float(abs(audio).max()) > 0.0  # NOT silent — regression guard for the
    # pretty_midi.synthesize()-zeroes-drum-tracks bug this function works around
