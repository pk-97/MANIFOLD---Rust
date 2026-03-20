"""Basic Pitch polyphonic pitch detection wrapper.

Wraps Spotify's Basic Pitch model for note-level detection on
bass and synth/pad stems. Provides onset time, duration, and
amplitude for each detected note.

Requires basic-pitch to be installed — raises on import failure.
"""

from __future__ import annotations

from pathlib import Path
from typing import List, Optional, Tuple

from manifold_audio.models import Event

# Basic Pitch note event tuple: (start_sec, end_sec, pitch_midi, amplitude, pitch_bend).
NoteEvent = Tuple[float, float, int, float]


def _compute_rms_envelope(audio_path: str, window_sec: float = 0.05):
    """Load audio and return (samples, sr, rms_db_array, hop_samples).

    rms_db_array[i] = RMS in dB for the window starting at i * hop_samples.
    """
    import librosa
    import numpy as np

    y, sr = librosa.load(audio_path, sr=None, mono=True)
    hop = int(sr * window_sec)
    if hop < 1:
        hop = 1

    # Pad so the last window is complete.
    pad_len = hop - (len(y) % hop) if len(y) % hop != 0 else 0
    if pad_len > 0:
        y_padded = np.pad(y, (0, pad_len), mode="constant")
    else:
        y_padded = y

    n_frames = len(y_padded) // hop
    rms = np.empty(n_frames, dtype=np.float32)
    for i in range(n_frames):
        frame = y_padded[i * hop : (i + 1) * hop]
        rms[i] = np.sqrt(np.mean(frame ** 2))

    # Convert to dB (floor at -100 dB to avoid -inf).
    rms_db = np.maximum(20.0 * np.log10(np.maximum(rms, 1e-10)), -100.0)
    return y, sr, rms_db, hop


def _filter_notes_by_energy(
    notes: List[NoteEvent],
    rms_db,
    hop_samples: int,
    sr: int,
    min_energy_db: float,
) -> List[NoteEvent]:
    """Drop notes whose onset falls in a window below min_energy_db."""
    result: List[NoteEvent] = []
    for note in notes:
        start_sec = note[0]
        frame_idx = int(start_sec * sr) // hop_samples
        frame_idx = min(frame_idx, len(rms_db) - 1)
        if rms_db[frame_idx] >= min_energy_db:
            result.append(note)
    return result


def detect_notes_basic_pitch(
    audio_path: str,
    onset_threshold: float = 0.5,
    frame_threshold: float = 0.3,
    min_note_length: float = 127.7,
    min_frequency: Optional[float] = None,
    max_frequency: Optional[float] = None,
    min_energy_db: Optional[float] = None,
) -> List[NoteEvent]:
    """Run Basic Pitch inference on an audio file.

    Parameters
    ----------
    audio_path : str
        Path to audio file (wav, mp3, etc.).
    onset_threshold : float
        Onset detection sensitivity (higher = stricter).
    frame_threshold : float
        Frame-level pitch detection threshold.
    min_note_length : float
        Minimum note length in milliseconds.
    min_frequency : float, optional
        Minimum detected frequency in Hz (None = no limit).
    max_frequency : float, optional
        Maximum detected frequency in Hz (None = no limit).
    min_energy_db : float, optional
        Minimum RMS energy (dB) at note onset.  Notes in windows
        quieter than this are discarded.  None = no energy gate.
        Typical values: -50 (aggressive) to -30 (conservative).

    Returns
    -------
    list[NoteEvent]
        List of (start_sec, end_sec, pitch_midi, amplitude).

    Raises
    ------
    ImportError
        If basic-pitch is not installed.
    FileNotFoundError
        If audio_path does not exist.
    """
    from basic_pitch.inference import predict

    audio_path = str(audio_path)
    if not Path(audio_path).is_file():
        raise FileNotFoundError(f"[basic_pitch] audio file not found: {audio_path}")

    # Basic Pitch's lowest note is A0 ≈ 27.5 Hz.  Values below that cause
    # the model to return zero results silently.  Clamp to 28 Hz floor.
    _BP_MIN_FREQ_FLOOR = 28.0
    if min_frequency is not None and 0 < min_frequency < _BP_MIN_FREQ_FLOOR:
        min_frequency = _BP_MIN_FREQ_FLOOR
    if max_frequency is not None and 0 < max_frequency < _BP_MIN_FREQ_FLOOR:
        max_frequency = _BP_MIN_FREQ_FLOOR

    _model_output, _midi_data, note_events = predict(
        audio_path,
        onset_threshold=onset_threshold,
        frame_threshold=frame_threshold,
        minimum_note_length=min_note_length,
        minimum_frequency=min_frequency,
        maximum_frequency=max_frequency,
    )

    # note_events: List[(start_sec, end_sec, pitch_midi, amplitude, pitch_bend)]
    # Strip pitch_bend (not needed for MANIFOLD).
    result: List[NoteEvent] = []
    for note in note_events:
        start = float(note[0])
        end = float(note[1])
        pitch = int(note[2])
        amp = float(note[3])
        if end > start:
            result.append((start, end, pitch, amp))

    # Windowed energy gate: discard notes in silent/bleed regions.
    if min_energy_db is not None and result:
        _, sr, rms_db, hop = _compute_rms_envelope(audio_path)
        before = len(result)
        result = _filter_notes_by_energy(result, rms_db, hop, sr, min_energy_db)
        dropped = before - len(result)
        if dropped > 0:
            import sys
            print(f"[basic_pitch] energy gate ({min_energy_db} dB): kept {len(result)}/{before} notes", file=sys.stderr)

    result.sort(key=lambda n: n[0])
    return result


def split_bass_by_duration(
    notes: List[NoteEvent],
    threshold_sec: float = 1.7144,
) -> Tuple[List[Event], List[Event]]:
    """Split bass notes into stabs (short) and sustained (long) by duration.

    Parameters
    ----------
    notes : list[NoteEvent]
        Output from detect_notes_basic_pitch().
    threshold_sec : float
        Notes shorter than this → "bass" (stab); longer or equal → "bass_sustained".
        Default ~1.7144s ≈ 4 beats at 140 BPM.

    Returns
    -------
    (bass_events, bass_sustained_events) : tuple[list[Event], list[Event]]
    """
    bass_events: List[Event] = []
    sustained_events: List[Event] = []

    for start, end, _pitch, amplitude in notes:
        dur = end - start
        if dur <= 0:
            continue

        is_sustained = dur >= threshold_sec
        event = Event(
            type="bass_sustained" if is_sustained else "bass",
            time=round(start, 4),
            confidence=round(max(0.0, min(1.0, amplitude)), 4),
            duration_sec=round(dur, 4),
        )

        if is_sustained:
            sustained_events.append(event)
        else:
            bass_events.append(event)

    bass_events.sort(key=lambda e: e.time)
    sustained_events.sort(key=lambda e: e.time)
    return bass_events, sustained_events


def classify_synth_notes(
    notes: List[NoteEvent],
    overlap_threshold_sec: float = 0.05,
) -> Tuple[List[Event], List[Event]]:
    """Classify notes by polyphony: monophonic → synth (lead), polyphonic → pad.

    For each note, counts how many other notes overlap it by more than
    overlap_threshold_sec. Zero overlaps → "synth". One or more → "pad".

    Parameters
    ----------
    notes : list[NoteEvent]
        Output from detect_notes_basic_pitch().
    overlap_threshold_sec : float
        Minimum overlap duration to count as polyphonic (avoids edge-case
        near-simultaneous note-off/note-on from being classified as chords).

    Returns
    -------
    (synth_events, pad_events) : tuple[list[Event], list[Event]]
    """
    # Filter and sort by start time.
    filtered = [(s, e, p, a) for s, e, p, a in notes if e > s]
    filtered.sort(key=lambda n: n[0])

    synth_events: List[Event] = []
    pad_events: List[Event] = []

    for i, (start_i, end_i, _pitch_i, amp_i) in enumerate(filtered):
        overlap_count = 0
        for j, (start_j, end_j, _pitch_j, _amp_j) in enumerate(filtered):
            if i == j:
                continue
            # Overlap = min(end_i, end_j) - max(start_i, start_j)
            overlap = min(end_i, end_j) - max(start_i, start_j)
            if overlap > overlap_threshold_sec:
                overlap_count += 1
                break  # One overlap is enough to classify as polyphonic.

        dur = end_i - start_i
        is_polyphonic = overlap_count > 0
        event = Event(
            type="pad" if is_polyphonic else "synth",
            time=round(start_i, 4),
            confidence=round(max(0.0, min(1.0, amp_i)), 4),
            duration_sec=round(dur, 4),
        )

        if is_polyphonic:
            pad_events.append(event)
        else:
            synth_events.append(event)

    synth_events.sort(key=lambda e: e.time)
    pad_events.sort(key=lambda e: e.time)
    return synth_events, pad_events
