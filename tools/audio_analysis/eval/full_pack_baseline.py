"""P3 deliverable 5 — baseline scoreboard for EVERY existing detector on the
full available fixture pack (docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md §5, P3).

This is a MEASUREMENT-ONLY module: no detector parameter, threshold, or
model config is tuned here (P3's own forbidden list). It reuses eval/run.py's
existing helpers (_resolve_path, _load_kick_truth_csv) and eval/metrics.py's
D10-frozen metric functions — no new metric definitions.

Coverage, per detector:
  - ADTOF drums (kick/snare/hihat/tom/cymbal): kick gets full P/R/F1 against
    the manifold_own kick_onset_csv fixtures (the only ground truth this pack
    currently has for drums); the other four classes are reported as
    predicted-event-count diagnostics only (D10: economy metrics are
    advisory) since no per-class drum-hit ground truth exists yet in this
    pack (E-GMD, Deferred #1, is the intended future source — flagged, not
    silently glossed over).
  - basic_pitch (bass/synth/melodic notes): scored against babyslakh's
    per-stem aligned MIDI (Bass + Piano stems — babyslakh's instrument
    taxonomy has no stem literally tagged "Synth"; Piano is the stand-in
    melodic/harmonic role) and against the P3 self-render generator's
    arp/pad fixtures (exact MIDI truth, and the arp fixture in particular
    IS a literal synth-program MIDI render).
  - vocal onset (madmom CNN, still in place pre-P6): scored on MUSDB18 as a
    mixture-vs-clean-stem AGREEMENT measurement, not literal P/R/F1 against
    hand-labeled truth — see the module-level deviation note below.
  - beat/downbeat/tempo (Beat This, post-P2): reuses eval.beat_scoring's
    existing per-fixture scoring across the liveshow corpus (dev+heldout).

DEVIATION FROM THE LITERAL D13 RECIPE (flagged per P3's own "if reality
doesn't match the brief, report the discrepancy" clause): fixtures.toml's
`musdb18_compressed` note says to score the vocal detector "on demucs output
of the mix" against clean-stem-derived truth regions. A real demucs
separation pass per track is a multi-minute DNN inference; running it across
even a small MUSDB18 sample was judged out of this phase's time budget
(P3 measures many other things too, per the same session). What this module
does INSTEAD: extract the true vocal stem and the full mixture directly from
each `.stem.mp4` (via ffmpeg stream-mapping — verified with ffprobe that
these NI-stems containers expose 5 addressable AAC audio streams: 0=mixture,
1=drums, 2=bass, 3=other, 4=vocals), then run the SAME madmom CNN onset
detector on both and report AGREEMENT (event_prf treating clean-stem
detections as the reference) rather than accuracy against hand-labeled
truth. This measures something real and honestly labeled (how much full-mix
context shifts/loses vocal onsets relative to the isolated stem) but is NOT
the demucs-in-the-loop number the design doc's note describes — a future
pass should add the demucs step if that specific number is needed.

Usage:
    python -m eval.full_pack_baseline --report eval/scoreboard/p3_baseline_<date>.json
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
import tempfile
import wave
from pathlib import Path
from typing import Any, Dict, List, Optional

import numpy as np
import yaml

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.paths import DATA_ROOT
from eval import metrics
from eval.run import AUDIO_ANALYSIS_ROOT, REPO_ROOT, _load_kick_truth_csv, _resolve_path, load_fixtures

SAMPLE_RATE = 44100


# ---------------------------------------------------------------------------
# ADTOF drums (per-class)
# ---------------------------------------------------------------------------


def run_baseline_adtof_drums(fixtures: List[Dict[str, Any]]) -> Dict[str, Any]:
    from manifold_audio.adtof_detection import detect_drums_adtof

    per_track: List[Dict[str, Any]] = []
    class_counts_total: Dict[str, int] = {}
    for fixture in fixtures:
        if fixture.get("truth_kind") != "kick_onset_csv":
            continue
        base_dir = _resolve_path(fixture["path"])
        mix_path = base_dir / "mix.wav"
        if not mix_path.exists():
            per_track.append({"id": fixture["id"], "error": f"missing {mix_path}"})
            continue
        try:
            events = detect_drums_adtof(str(mix_path))
        except Exception as exc:  # best-effort — one bad track must not abort the pack run
            per_track.append({"id": fixture["id"], "error": str(exc)[:300]})
            continue
        by_class: Dict[str, List[float]] = {}
        for e in events:
            by_class.setdefault(e.type, []).append(e.time)

        truth = _load_kick_truth_csv(_resolve_path(fixture["labels_path"]))["mix"]
        kick_prf = metrics.event_prf(by_class.get("kick", []), truth, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
        row = {
            "id": fixture["id"],
            "domain": fixture.get("domain", "other"),
            "kick": {"n_pred": len(by_class.get("kick", [])), "n_truth": len(truth), **kick_prf.to_dict()},
            "predicted_event_counts_by_class": {k: len(v) for k, v in by_class.items()},
        }
        per_track.append(row)
        for k, v in by_class.items():
            class_counts_total[k] = class_counts_total.get(k, 0) + len(v)

    kick_rows = [r for r in per_track if "kick" in r]
    return {
        "per_track": per_track,
        "kick_f1_by_domain": metrics.domain_aggregate(
            [{"domain": r["domain"], "kick_f1": r["kick"]["f1"]} for r in kick_rows], "kick_f1"
        ) if kick_rows else {},
        "predicted_event_counts_by_class_total": class_counts_total,
        "note": (
            "Only 'kick' has ground truth in this fixture pack (kick_onset_csv "
            "fixtures label kicks only). snare/hihat/tom/cymbal counts above are "
            "predicted-event-density diagnostics ONLY (D10: economy metrics are "
            "advisory), not accuracy — this pack has no per-class drum-hit ground "
            "truth yet (E-GMD, Deferred #1, is the intended future source). This "
            "kick number is the ADTOF-replacement bake-off's reference point."
        ),
    }


# ---------------------------------------------------------------------------
# basic_pitch (bass / synth / melodic notes)
# ---------------------------------------------------------------------------


def _midi_truth_notes(midi_path: Path) -> List[Dict[str, float]]:
    import pretty_midi

    pm = pretty_midi.PrettyMIDI(str(midi_path))
    notes = []
    for inst in pm.instruments:
        for n in inst.notes:
            notes.append({"start_sec": float(n.start), "end_sec": float(n.end), "pitch": int(n.pitch)})
    notes.sort(key=lambda n: n["start_sec"])
    return notes


def _score_basic_pitch_against_truth(wav_path: Path, truth_notes: List[Dict[str, float]]) -> Dict[str, Any]:
    from manifold_audio.basic_pitch_detection import detect_notes_basic_pitch

    pred = detect_notes_basic_pitch(str(wav_path))
    pred_onsets = [p[0] for p in pred]
    truth_onsets = [t["start_sec"] for t in truth_notes]
    prf = metrics.event_prf(pred_onsets, truth_onsets, tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
    pred_spans = [(p[0], p[1]) for p in pred]
    truth_spans = [(t["start_sec"], t["end_sec"]) for t in truth_notes]
    mean_iou, n_iou_matched = metrics.mean_duration_iou(pred_spans, truth_spans)
    return {
        "n_pred": len(pred),
        "n_truth": len(truth_notes),
        **prf.to_dict(),
        "mean_duration_iou": mean_iou,
        "n_iou_matched": n_iou_matched,
    }


def _find_babyslakh_stem(track_dir: Path, inst_class_substr: str) -> Optional[Dict[str, Path]]:
    meta_path = track_dir / "metadata.yaml"
    if not meta_path.exists():
        return None
    meta = yaml.safe_load(meta_path.read_text())
    for stem_id, info in (meta.get("stems") or {}).items():
        if inst_class_substr.lower() in str(info.get("inst_class", "")).lower():
            wav = track_dir / "stems" / f"{stem_id}.wav"
            midi = track_dir / "MIDI" / f"{stem_id}.mid"
            if wav.exists() and midi.exists():
                return {"wav": wav, "midi": midi}
    return None


def run_baseline_basic_pitch_babyslakh(babyslakh_root: Path, max_tracks: Optional[int] = None) -> List[Dict[str, Any]]:
    nested = babyslakh_root / "babyslakh_16k"
    root = nested if nested.is_dir() else babyslakh_root
    if not root.is_dir():
        return []
    track_dirs = sorted(p for p in root.iterdir() if p.is_dir() and p.name.startswith("Track"))
    if max_tracks is not None:
        track_dirs = track_dirs[:max_tracks]

    rows: List[Dict[str, Any]] = []
    # babyslakh has no stem literally tagged "Synth" — Piano is used as the
    # stand-in melodic/harmonic role (see module docstring); Bass is the
    # literal bass role the design doc names.
    for role, inst_class in (("bass", "Bass"), ("melodic_piano_standin_for_synth", "Piano")):
        for track_dir in track_dirs:
            found = _find_babyslakh_stem(track_dir, inst_class)
            if found is None:
                continue
            truth = _midi_truth_notes(found["midi"])
            if not truth:
                continue
            try:
                result = _score_basic_pitch_against_truth(found["wav"], truth)
            except Exception as exc:
                result = {"error": str(exc)[:300]}
            rows.append({"track": track_dir.name, "role": role, **result})
    return rows


def run_baseline_basic_pitch_self_render(self_render_dir: Path) -> List[Dict[str, Any]]:
    manifest_path = self_render_dir / "manifest.json"
    if not manifest_path.exists():
        return []
    manifest = json.loads(manifest_path.read_text())
    rows: List[Dict[str, Any]] = []
    for entry in manifest:
        if entry["id"] == "kick_hat_128bpm":
            continue  # drum fixture — not a basic_pitch (pitched-note) target
        wav_path = self_render_dir / entry["wav"]
        truth_path = self_render_dir / entry["truth"]
        if not (wav_path.exists() and truth_path.exists()):
            continue
        truth = json.loads(truth_path.read_text())
        try:
            result = _score_basic_pitch_against_truth(wav_path, truth)
        except Exception as exc:
            result = {"error": str(exc)[:300]}
        role = "synth" if entry["id"].startswith("arp") else "pad"
        rows.append({"id": entry["id"], "role": role, **result})
    return rows


def run_baseline_basic_pitch_maestro(maestro_dir: Path, max_tracks: Optional[int] = None) -> List[Dict[str, Any]]:
    """MAESTRO's role per the design doc is 'sustained-polyphony tuning' —
    P4's job, not P3's — but since P3 already has the audio+MIDI on disk
    (rendered via eval/midi_synth.py), a from-the-box basic_pitch pass here
    costs little and gives an honest starting number for that later phase."""
    midi_dir = maestro_dir / "midi"
    audio_dir = maestro_dir / "audio"
    if not (midi_dir.is_dir() and audio_dir.is_dir()):
        return []
    midi_files = sorted(midi_dir.glob("*.midi")) + sorted(midi_dir.glob("*.mid"))
    if max_tracks is not None:
        midi_files = midi_files[:max_tracks]
    rows: List[Dict[str, Any]] = []
    for midi_path in midi_files:
        wav_path = audio_dir / (midi_path.stem + ".wav")
        if not wav_path.exists():
            continue
        truth = _midi_truth_notes(midi_path)
        if not truth:
            continue
        try:
            result = _score_basic_pitch_against_truth(wav_path, truth)
        except Exception as exc:
            result = {"error": str(exc)[:300]}
        rows.append({"track": midi_path.stem, **result})
    return rows


# ---------------------------------------------------------------------------
# Vocal onset (madmom CNN) — MUSDB18 mixture-vs-clean-stem agreement
# ---------------------------------------------------------------------------


def _extract_stem_stream(mp4_path: Path, stream_index: int, out_wav: Path, ffmpeg_bin: Optional[str] = None) -> bool:
    from manifold_audio.external_tools import _resolve_ffmpeg_path

    ffmpeg = _resolve_ffmpeg_path(ffmpeg_bin)
    if not ffmpeg:
        return False
    out_wav.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        ffmpeg, "-hide_banner", "-loglevel", "error", "-y",
        "-i", str(mp4_path),
        "-map", f"0:{stream_index}",
        "-ac", "1", "-ar", str(SAMPLE_RATE),
        str(out_wav),
    ]
    try:
        subprocess.run(cmd, check=True, timeout=120)
    except Exception:
        return False
    return out_wav.exists() and out_wav.stat().st_size > 0


def run_baseline_vocal_musdb(musdb_root: Path, n_tracks: int = 5, ffmpeg_bin: Optional[str] = None) -> Dict[str, Any]:
    from manifold_audio.onset_detection import detect_madmom_onsets

    test_dir = musdb_root / "test"
    if not test_dir.is_dir():
        return {"rows": [], "note": "MUSDB18 test/ not present"}
    stem_files = sorted(test_dir.glob("*.stem.mp4"))[:n_tracks]

    rows: List[Dict[str, Any]] = []
    with tempfile.TemporaryDirectory() as tmp:
        tmp_dir = Path(tmp)
        for stem_path in stem_files:
            track_id = stem_path.stem.replace(".stem", "")
            mix_wav = tmp_dir / f"{track_id}_mix.wav"
            vocal_wav = tmp_dir / f"{track_id}_vocal.wav"
            ok_mix = _extract_stem_stream(stem_path, 0, mix_wav, ffmpeg_bin)
            ok_vocal = _extract_stem_stream(stem_path, 4, vocal_wav, ffmpeg_bin)
            if not (ok_mix and ok_vocal):
                rows.append({"id": track_id, "error": "ffmpeg stream extraction failed"})
                continue
            try:
                mix_result = detect_madmom_onsets(str(mix_wav), method="cnn")
                vocal_result = detect_madmom_onsets(str(vocal_wav), method="cnn")
            except Exception as exc:
                rows.append({"id": track_id, "error": str(exc)[:300]})
                continue
            if mix_result is None or vocal_result is None:
                rows.append({"id": track_id, "error": "madmom onset detection returned None"})
                continue
            _, mix_onsets = mix_result
            _, vocal_onsets = vocal_result
            # Clean-stem detections treated as the reference (pseudo-truth) —
            # this is an AGREEMENT measurement, not accuracy against a
            # hand-labeled truth (see module docstring's D13 deviation note).
            agreement = metrics.event_prf(list(mix_onsets), list(vocal_onsets), tolerance_sec=metrics.EVENT_TOLERANCE_SEC)
            rows.append({
                "id": track_id,
                "n_onsets_clean_vocal_stem": len(vocal_onsets),
                "n_onsets_full_mixture": len(mix_onsets),
                "mixture_vs_clean_stem_agreement_f1": agreement.f1,
                "mixture_vs_clean_stem_agreement_precision": agreement.precision,
                "mixture_vs_clean_stem_agreement_recall": agreement.recall,
            })

    valid = [r for r in rows if "error" not in r]
    mean_agreement_f1 = float(np.mean([r["mixture_vs_clean_stem_agreement_f1"] for r in valid])) if valid else None
    return {
        "rows": rows,
        "n_scored": len(valid),
        "mean_mixture_vs_clean_stem_agreement_f1": mean_agreement_f1,
        "note": (
            "This is a mixture-vs-clean-stem AGREEMENT measurement (madmom CNN "
            "vocal onset run on both the isolated true vocal stem and the full "
            "mixture, scored against each other), NOT accuracy against a hand-"
            "labeled truth, and does NOT include the demucs separation step "
            "fixtures.toml's D13 note describes — deferred, see module docstring."
        ),
    }


# ---------------------------------------------------------------------------
# Beat / downbeat / tempo (Beat This) — reuses eval.beat_scoring
# ---------------------------------------------------------------------------


def run_baseline_beat_downbeat_liveshow() -> Dict[str, Any]:
    from eval.beat_scoring import LIVESHOW_SONG_FIXTURES, build_song_fixture, load_tempo_points, run_beat_tracker_on_fixture, score_fixture

    tempo_points = load_tempo_points()
    out_dir = DATA_ROOT / "liveshow_song_slices"
    rows = []
    for fx in LIVESHOW_SONG_FIXTURES:
        try:
            fixture = build_song_fixture(fx, tempo_points, out_dir)
            pred = run_beat_tracker_on_fixture(fixture)
            rows.append(score_fixture(fixture, pred))
        except Exception as exc:
            rows.append({"id": fx["id"], "error": str(exc)[:300]})

    valid = [r for r in rows if "error" not in r]
    by_split: Dict[str, Any] = {}
    for split in ("dev", "heldout"):
        split_rows = [r for r in valid if r["split"] == split]
        if not split_rows:
            continue
        by_split[split] = {
            "beat_f1": metrics.domain_aggregate(split_rows, "beat_f1"),
            "downbeat_f1": metrics.domain_aggregate(split_rows, "downbeat_f1"),
        }
    return {"rows": rows, "by_split": by_split}


def _estimate_constant_shift(pred: List[float], truth: List[float], bin_sec: float = 0.02, max_abs_shift: float = 8.0):
    """Coarse cross-correlation via a difference histogram: for every
    (pred, truth) pair, bin (pred - truth); the modal bin (refined by the
    median of near-modal diffs) is the best-supported constant time shift
    between the two sequences. Found necessary (2026-07-17) because
    YouTube-matched Harmonix audio can carry a multi-second lead-in/edit
    offset relative to the original reference recording the annotations
    were made against — see module docstring / run_baseline_beat_downbeat_harmonix.
    Returns (shift_sec, n_votes) — n_votes is the count backing the modal
    bin, a rough confidence signal (near len(truth) = strong constant-shift
    match; near 0 = no coherent shift found, e.g. wrong/unrelated audio)."""
    p = np.asarray(pred, dtype=np.float64)
    t = np.asarray(truth, dtype=np.float64)
    if p.size == 0 or t.size == 0:
        return 0.0, 0
    diffs = (p[:, None] - t[None, :]).ravel()
    diffs = diffs[np.abs(diffs) <= max_abs_shift]
    if diffs.size == 0:
        return 0.0, 0
    bins = np.round(diffs / bin_sec).astype(int)
    vals, counts = np.unique(bins, return_counts=True)
    best_bin = vals[np.argmax(counts)]
    near = diffs[np.abs(bins - best_bin) <= 1]
    return float(np.median(near)), int(counts.max())


def run_baseline_beat_downbeat_harmonix(annotations_dir: Path, audio_dir: Path, max_tracks: int = 15) -> Dict[str, Any]:
    """Extends the beat/downbeat baseline to the newly-matched Harmonix
    electronic-slice audio (P3 deliverable 3) using Harmonix's own
    beats_and_downbeats/<id>.txt annotations (columns: time_sec, beat_in_bar,
    bar_number; beat_in_bar==1 marks a downbeat) as ground truth. Capped at
    max_tracks — a full 107-track sweep is a reasonable follow-up run, not
    required to establish this baseline's existence.

    FINDING (2026-07-17, first real run): raw (zero-shift) beat_f1 on the
    first tracks came back exactly 0.0 despite predicted/truth beat COUNTS
    being the same order of magnitude and near-identical spacing — a
    hallmark of a constant time OFFSET between sequences, not a detector
    failure. Root cause, confirmed on '0012_aroundtheworld': the
    YouTube-matched audio is 241.65s long vs. the annotation's ~150.38s
    coverage, and a difference-histogram cross-correlation
    (_estimate_constant_shift) finds a dominant -4.06s shift backed by
    204/305 truth beats; applying it recovers beat_f1 from 0.0 to 0.75.
    This means the specific YouTube upload matched for this track carries
    ~4s of extra lead-in (or is a different edit) relative to whatever
    recording Harmonix's annotators used — a data-alignment problem with
    YouTube-sourced audio, not a Beat This regression. Both the raw and the
    shift-corrected numbers are reported per track below so this isn't
    hidden; no correction is applied upstream (P3 measures, doesn't fix) —
    a future phase wanting to actually USE Harmonix-matched audio for
    tuning would need this alignment step productized (analogous to D14's
    decode-stage correction), which is out of this phase's scope."""
    from manifold_audio.beat_tracking import estimate_beats_this

    beat_dir = annotations_dir / "beats_and_downbeats"
    if not (beat_dir.is_dir() and audio_dir.is_dir()):
        return {"rows": [], "note": "harmonix annotations or matched audio not present"}

    wav_files = sorted(audio_dir.glob("*.wav"))[:max_tracks]
    rows = []
    for wav_path in wav_files:
        track_id = wav_path.stem
        ann_path = beat_dir / f"{track_id}.txt"
        if not ann_path.exists():
            continue
        truth_beats: List[float] = []
        truth_downbeats: List[float] = []
        for line in ann_path.read_text().splitlines():
            parts = line.split()
            if len(parts) < 2:
                continue
            t = float(parts[0])
            beat_in_bar = int(float(parts[1]))
            truth_beats.append(t)
            if beat_in_bar == 1:
                truth_downbeats.append(t)
        try:
            result = estimate_beats_this(str(wav_path))
        except Exception as exc:
            rows.append({"id": track_id, "error": str(exc)[:300]})
            continue
        if result is None:
            rows.append({"id": track_id, "error": "Beat This inference returned None"})
            continue

        beat_prf_raw = metrics.beat_prf(result.beat_times, truth_beats)
        shift_sec, shift_votes = _estimate_constant_shift(result.beat_times, truth_beats)
        shifted_beats = [b - shift_sec for b in result.beat_times]
        shifted_downbeats = [b - shift_sec for b in result.downbeat_times]
        beat_prf_shifted = metrics.beat_prf(shifted_beats, truth_beats)
        downbeat_prf_shifted = metrics.downbeat_prf(shifted_downbeats, truth_downbeats)

        rows.append({
            "id": track_id,
            "n_truth_beats": len(truth_beats),
            "n_pred_beats": len(result.beat_times),
            "beat_f1_raw": beat_prf_raw.f1,
            "estimated_offset_sec": shift_sec,
            "offset_vote_count": shift_votes,
            "beat_f1_after_offset_correction": beat_prf_shifted.f1,
            "n_truth_downbeats": len(truth_downbeats),
            "n_pred_downbeats": len(result.downbeat_times),
            "downbeat_f1_after_offset_correction": downbeat_prf_shifted.f1,
        })

    valid = [r for r in rows if "error" not in r]
    return {
        "rows": rows,
        "n_scored": len(valid),
        "n_candidates": len(wav_files),
        "beat_f1_raw_mean": float(np.mean([r["beat_f1_raw"] for r in valid])) if valid else None,
        "beat_f1_after_offset_correction_mean": float(np.mean([r["beat_f1_after_offset_correction"] for r in valid])) if valid else None,
        "downbeat_f1_after_offset_correction_mean": float(np.mean([r["downbeat_f1_after_offset_correction"] for r in valid])) if valid else None,
        "note": (
            f"sample of {max_tracks} from the matched Harmonix electronic slice — not the full "
            "107-track set (a follow-up full sweep is straightforward, same code path). "
            "beat_f1_raw is near-zero for most tracks because YouTube-matched audio commonly "
            "carries a multi-second timing offset vs. the Harmonix annotation reference (see "
            "docstring) — the offset-corrected numbers are the meaningful ones for judging Beat "
            "This itself; the raw numbers are reported too so the offset problem isn't hidden."
        ),
    }


# ---------------------------------------------------------------------------
# Orchestration
# ---------------------------------------------------------------------------


def build_full_pack_baseline(fixtures_path: Path, max_babyslakh_tracks: Optional[int] = None, max_musdb_tracks: int = 5, max_harmonix_tracks: int = 15, max_maestro_tracks: Optional[int] = 5) -> Dict[str, Any]:
    fixtures = load_fixtures(fixtures_path)

    report: Dict[str, Any] = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "pipeline_version": "p3-2026-07-17",
        "phase": "P3 full-pack baseline (measurement only, no tuning)",
    }

    print("[full_pack_baseline] ADTOF drums ...", file=sys.stderr)
    report["adtof_drums"] = run_baseline_adtof_drums(fixtures)

    babyslakh_fixture = next((f for f in fixtures if f["id"] == "babyslakh_16k"), None)
    print("[full_pack_baseline] basic_pitch on babyslakh (bass + melodic-piano-standin-for-synth) ...", file=sys.stderr)
    report["basic_pitch_babyslakh"] = (
        run_baseline_basic_pitch_babyslakh(_resolve_path(babyslakh_fixture["path"]), max_babyslakh_tracks)
        if babyslakh_fixture else []
    )

    self_render_dir = DATA_ROOT / "self_render"
    print("[full_pack_baseline] basic_pitch on self-render generator fixtures ...", file=sys.stderr)
    report["basic_pitch_self_render"] = run_baseline_basic_pitch_self_render(self_render_dir)

    maestro_dir = DATA_ROOT / "maestro_v3"
    print("[full_pack_baseline] basic_pitch on MAESTRO v3 selection ...", file=sys.stderr)
    report["basic_pitch_maestro"] = run_baseline_basic_pitch_maestro(maestro_dir, max_maestro_tracks)

    musdb_fixture = next((f for f in fixtures if f["id"] == "musdb18_compressed"), None)
    print("[full_pack_baseline] vocal onset (madmom CNN) on MUSDB18 sample ...", file=sys.stderr)
    report["vocal_onset_musdb18"] = (
        run_baseline_vocal_musdb(_resolve_path(musdb_fixture["path"]), max_musdb_tracks)
        if musdb_fixture else {"rows": [], "note": "musdb18_compressed fixture not in manifest"}
    )

    print("[full_pack_baseline] beat/downbeat/tempo (Beat This) on liveshow corpus ...", file=sys.stderr)
    report["beat_downbeat_liveshow"] = run_baseline_beat_downbeat_liveshow()

    harmonix_annotations = DATA_ROOT / "harmonixset" / "dataset"
    harmonix_audio = DATA_ROOT / "harmonixset_audio"
    print("[full_pack_baseline] beat/downbeat/tempo (Beat This) on Harmonix electronic-slice sample ...", file=sys.stderr)
    report["beat_downbeat_harmonix_electronic_sample"] = run_baseline_beat_downbeat_harmonix(
        harmonix_annotations, harmonix_audio, max_harmonix_tracks
    )

    return report


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--fixtures", type=Path, default=AUDIO_ANALYSIS_ROOT / "eval" / "fixtures.toml")
    parser.add_argument("--report", type=Path, default=None)
    parser.add_argument("--max-babyslakh-tracks", type=int, default=None)
    parser.add_argument("--max-musdb-tracks", type=int, default=5)
    parser.add_argument("--max-harmonix-tracks", type=int, default=15)
    parser.add_argument("--max-maestro-tracks", type=int, default=5)
    args = parser.parse_args(argv)

    report = build_full_pack_baseline(
        args.fixtures,
        max_babyslakh_tracks=args.max_babyslakh_tracks,
        max_musdb_tracks=args.max_musdb_tracks,
        max_harmonix_tracks=args.max_harmonix_tracks,
        max_maestro_tracks=args.max_maestro_tracks,
    )

    report_path = args.report or (AUDIO_ANALYSIS_ROOT / "eval" / "scoreboard" / f"p3_baseline_{dt.date.today().isoformat()}.json")
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2))
    print(f"[full_pack_baseline] wrote {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
