"""CLI entry point, argument parsing, and progress emission."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path
from typing import Dict, Optional, Sequence

from manifold_audio.analyzer import analyze_percussion, build_output
from manifold_audio.audio_io import load_audio_mono
from manifold_audio.external_tools import (
    _build_demucs_cache_key,
    _resolve_demucs_command,
    _resolve_requested_demucs_stems,
    _separate_all_stems_demucs,
)
from manifold_audio.math_utils import _clamp
from manifold_audio.models import AnalysisConfig


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate MANIFOLD percussion JSON from audio.")
    parser.add_argument("input", type=Path, help="Path to input audio file")
    parser.add_argument("-o", "--output", type=Path, default=None, help="Output JSON path")
    parser.add_argument("--track-id", type=str, default=None, help="Track ID override")
    parser.add_argument("--sample-rate", type=int, default=44100, help="Analysis sample rate")
    parser.add_argument("--frame-size", type=int, default=1024, help="FFT frame size")
    parser.add_argument("--hop-size", type=int, default=256, help="Hop size")
    parser.add_argument("--min-bpm", type=float, default=55.0, help="Min BPM for beat tracking (default: 55)")
    parser.add_argument("--max-bpm", type=float, default=215.0, help="Max BPM for beat tracking (default: 215)")
    parser.add_argument(
        "--ffmpeg-bin",
        type=str,
        default=None,
        help="Optional absolute path to ffmpeg binary",
    )
    parser.add_argument(
        "--use-drum-stem",
        type=str,
        default="auto",
        help="Drum stem separation mode: auto, on, off",
    )
    parser.add_argument(
        "--demucs-bin",
        type=str,
        default=None,
        help="Optional absolute path to demucs binary",
    )
    parser.add_argument(
        "--demucs-model",
        type=str,
        default="htdemucs",
        help="Demucs model name when drum stem mode is enabled",
    )
    parser.add_argument(
        "--demucs-shifts",
        type=int,
        default=1,
        help="Demucs equivariant shifts (higher = slower, often better quality)",
    )
    parser.add_argument(
        "--demucs-overlap",
        type=float,
        default=0.25,
        help="Demucs chunk overlap ratio (0.05..0.95)",
    )
    parser.add_argument(
        "--demucs-device",
        type=str,
        default="auto",
        help="Demucs device: auto, mps, cuda, cpu",
    )
    parser.add_argument(
        "--demucs-segment",
        type=float,
        default=0.0,
        help="Demucs segment length in seconds (<=0 uses model default)",
    )
    parser.add_argument(
        "--demucs-no-split",
        type=str,
        default="off",
        help="Demucs no-split mode: on, off (default: off — on crashes for tracks longer than ~8s)",
    )
    parser.add_argument(
        "--demucs-jobs",
        type=int,
        default=0,
        help="Demucs worker jobs (0 lets Demucs decide)",
    )
    parser.add_argument(
        "--demucs-cache-dir",
        type=Path,
        default=None,
        help="Optional directory to persist demucs stems for re-analysis and auditioning",
    )
    parser.add_argument(
        "--reuse-demucs-cache",
        type=str,
        default="on",
        help="Reuse cached demucs stems when available: on, off",
    )
    parser.add_argument(
        "--emit-bass",
        type=str,
        default="off",
        help="Bass gesture event output mode: on, off",
    )
    parser.add_argument(
        "--use-bass-stem",
        type=str,
        default="auto",
        help="Bass stem separation mode: auto, on, off",
    )
    parser.add_argument(
        "--profile",
        type=str,
        default="electronic",
        help="Detection profile (single mode): electronic",
    )
    parser.add_argument(
        "--bass-profile",
        type=str,
        default="electronic",
        help="Bass profile (single mode): electronic",
    )
    parser.add_argument(
        "--use-vocal-stem",
        type=str,
        default="auto",
        help="Vocal stem separation mode: auto, on, off",
    )
    parser.add_argument(
        "--bass-sub-weight",
        type=float,
        default=None,
        help="Override bass profile sub-band weight (>=0)",
    )
    parser.add_argument(
        "--bass-body-weight",
        type=float,
        default=None,
        help="Override bass profile body-band weight (>=0)",
    )
    parser.add_argument(
        "--bass-bite-weight",
        type=float,
        default=None,
        help="Override bass profile bite-band weight (>=0)",
    )
    parser.add_argument(
        "--max-events",
        type=int,
        default=0,
        help="Limit event count after sorting (0 = unlimited)",
    )
    parser.add_argument(
        "--bpm-only",
        type=str,
        default="off",
        help="BPM-only mode: on, off. Skips stem separation and onset detection, "
        "only runs beat tracking and outputs BPM + beat grid (no events).",
    )
    parser.add_argument(
        "--config-file",
        type=Path,
        default=None,
        help="Optional JSON config file with detection parameter overrides",
    )
    parser.add_argument(
        "--instruments",
        type=str,
        default="drums,bass,synth,pad,vocal",
        help="Comma-separated instrument groups to analyze: drums, bass, synth, pad, vocal. "
        "Default: all. Example: --instruments drums,bass",
    )
    return parser.parse_args(argv)


def emit_progress(progress: float, message: str) -> None:
    clamped = max(0.0, min(1.0, float(progress)))
    text = str(message or "").strip() or "working"
    print(f"MANIFOLD_PROGRESS|{clamped:.3f}|{text}", flush=True)


def _load_and_compute_spectral(input_path, target_sr, ffmpeg_bin, frame_size, hop_size):
    """Load audio and compute band onsets for BPM refinement (picklable top-level)."""
    from manifold_audio.audio_io import load_audio_mono
    from manifold_audio.spectral import compute_band_onsets

    audio, sr = load_audio_mono(input_path, target_sr=target_sr, ffmpeg_bin=ffmpeg_bin)
    rough_bands = {
        "kick": (30.0, 180.0),
        "snare": (180.0, 2800.0),
        "hat": (4200.0, 15000.0),
        "perc": (900.0, 8500.0),
    }
    onsets, _rms, _hop = compute_band_onsets(audio, sr, frame_size, hop_size, rough_bands)
    global_onset = onsets["kick"] + onsets["snare"] + (0.5 * onsets["hat"]) + (0.5 * onsets["perc"])
    hop_time = hop_size / float(sr)
    duration_sec = float(len(audio)) / float(sr)
    return global_onset, hop_time, duration_sec


def _run_bpm_only(args: argparse.Namespace, detection_config: Optional[AnalysisConfig]) -> int:
    """Lightweight BPM + beat grid detection — no stems, no onset classification."""
    from concurrent.futures import ProcessPoolExecutor

    from manifold_audio.bpm import (
        _build_beat_grid,
        _detect_madmom_downbeat_phase,
        _estimate_madmom_beats,
        _refine_bpm_via_autocorrelation,
        _score_octave_hypotheses,
    )

    emit_progress(0.10, "tracking beats (RNN)")
    audio_path = str(args.input)

    try:
        madmom_bpm, beat_times, tempo_hypotheses = _estimate_madmom_beats(
            audio_path=audio_path,
            min_bpm=args.min_bpm,
            max_bpm=args.max_bpm,
            ffmpeg_bin=args.ffmpeg_bin,
        )
    except Exception as exc:
        print(f"ERROR: beat tracking failed: {exc}", file=sys.stderr)
        return 1

    if madmom_bpm is None or len(beat_times) < 2:
        print("ERROR: could not detect BPM from audio", file=sys.stderr)
        return 1

    # Run downbeat detection and spectral analysis in parallel — they're independent
    # after beat tracking completes. Downbeat needs beat_times; spectral needs only audio.
    emit_progress(0.40, "analysing downbeats + spectral in parallel")
    with ProcessPoolExecutor(max_workers=2) as pool:
        downbeat_future = pool.submit(
            _detect_madmom_downbeat_phase,
            audio_path=audio_path,
            beat_times=beat_times,
            beats_per_bar=4,
            ffmpeg_bin=args.ffmpeg_bin,
        )
        spectral_future = pool.submit(
            _load_and_compute_spectral,
            input_path=args.input,
            target_sr=args.sample_rate,
            ffmpeg_bin=args.ffmpeg_bin,
            frame_size=args.frame_size,
            hop_size=args.hop_size,
        )
        downbeat_phase = downbeat_future.result()
        global_onset, hop_time, duration_sec = spectral_future.result()

    emit_progress(0.70, "refining BPM")
    bpm = _score_octave_hypotheses(
        base_bpm=madmom_bpm,
        beat_times=beat_times,
        kick_events=[],
        snare_events=[],
        global_onset=global_onset,
        hop_time=hop_time,
        duration_sec=duration_sec,
        min_bpm=args.min_bpm,
        max_bpm=args.max_bpm,
        tempo_hypotheses=tempo_hypotheses,
        detection_config=detection_config,
    )
    if isinstance(bpm, tuple):
        bpm, beat_times = bpm

    cfg = detection_config
    bpm = _refine_bpm_via_autocorrelation(
        candidate_bpm=bpm,
        global_onset=global_onset,
        hop_time=hop_time,
        search_half_range=cfg.autocorr_search_half_range if cfg and cfg.autocorr_search_half_range is not None else 4,
        margin_threshold=cfg.autocorr_margin_threshold if cfg and cfg.autocorr_margin_threshold is not None else 0.01,
    )

    emit_progress(0.80, "building beat grid")
    beat_grid = _build_beat_grid(
        duration_sec=duration_sec,
        bpm=bpm,
        global_onset=global_onset,
        hop_time=hop_time,
        kick_events=[],
        phase_offset_sec=0.0,
        tracker_beat_times=beat_times,
        mode_override="madmom",
        min_bpm=args.min_bpm,
        max_bpm=args.max_bpm,
        madmom_downbeat_phase=downbeat_phase,
        detection_config=detection_config,
    )

    bpm_confidence = 0.0
    if beat_grid is not None:
        bpm_confidence = float(_clamp(beat_grid.confidence, 0.0, 1.0))
        if beat_grid.bpm_derived is not None and beat_grid.bpm_derived > 0:
            bpm = beat_grid.bpm_derived

    emit_progress(0.90, "writing BPM analysis JSON")
    from manifold_audio.analyzer import build_output

    track_id = args.track_id or args.input.stem
    payload = build_output(
        track_id=track_id,
        bpm=bpm,
        bpm_confidence=bpm_confidence,
        beat_grid=beat_grid,
        events=[],
    )

    output_path = args.output or args.input.with_suffix(".percussion.json")
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    bpm_text = f"{bpm:.2f}" if bpm is not None else "n/a"
    bpm_conf_text = f"{(100.0 * bpm_confidence):.1f}%"
    print(f"BPM-only analysis: {bpm_text} BPM (confidence {bpm_conf_text})")
    if beat_grid is not None:
        print(
            f"Beat grid: mode={beat_grid.mode}, beats={len(beat_grid.beat_times)}, "
            f"downbeats={len(beat_grid.downbeat_indices)}"
        )
    print(f"Wrote -> {output_path}")
    emit_progress(1.00, "BPM analysis finished")
    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    emit_progress(0.03, "validating analysis settings")

    # Load detection config from JSON file if provided.
    detection_config: Optional[AnalysisConfig] = None
    if args.config_file is not None:
        try:
            import json as _json
            config_data = _json.loads(args.config_file.read_text(encoding="utf-8-sig"))
            detection_config = AnalysisConfig.from_json(config_data)
        except Exception as config_exc:
            print(f"WARN: failed to load config file: {config_exc}", file=sys.stderr)

    valid_instruments = {"drums", "bass", "synth", "pad", "vocal"}
    instruments = frozenset(
        s.strip().lower() for s in str(args.instruments).split(",") if s.strip()
    )
    unknown = instruments - valid_instruments
    if unknown:
        print(
            f"ERROR: unknown instruments: {', '.join(sorted(unknown))}. "
            f"Valid: {', '.join(sorted(valid_instruments))}",
            file=sys.stderr,
        )
        return 2
    if not instruments:
        instruments = frozenset(valid_instruments)

    if args.frame_size <= 0 or args.hop_size <= 0:
        print("ERROR: frame-size and hop-size must be > 0", file=sys.stderr)
        return 2
    if args.sample_rate < 8000:
        print("ERROR: sample-rate must be >= 8000", file=sys.stderr)
        return 2
    if not (20.0 <= args.min_bpm < args.max_bpm <= 300.0):
        print("ERROR: --min-bpm must be in [20,300], --max-bpm must be in [20,300], and min < max", file=sys.stderr)
        return 2

    bpm_only = str(args.bpm_only).strip().lower()
    if bpm_only not in {"on", "off"}:
        print("ERROR: --bpm-only must be one of: on, off", file=sys.stderr)
        return 2

    if bpm_only == "on":
        return _run_bpm_only(args, detection_config)

    stem_mode = str(args.use_drum_stem).strip().lower()
    if stem_mode not in {"auto", "on", "off"}:
        print("ERROR: --use-drum-stem must be one of: auto, on, off", file=sys.stderr)
        return 2

    emit_bass = str(args.emit_bass).strip().lower()
    if emit_bass not in {"on", "off"}:
        print("ERROR: --emit-bass must be one of: on, off", file=sys.stderr)
        return 2

    bass_stem_mode = str(args.use_bass_stem).strip().lower()
    if bass_stem_mode not in {"auto", "on", "off"}:
        print("ERROR: --use-bass-stem must be one of: auto, on, off", file=sys.stderr)
        return 2

    vocal_stem_mode = str(args.use_vocal_stem).strip().lower()
    if vocal_stem_mode not in {"auto", "on", "off"}:
        print("ERROR: --use-vocal-stem must be one of: auto, on, off", file=sys.stderr)
        return 2

    reuse_demucs_cache = str(args.reuse_demucs_cache).strip().lower()
    if reuse_demucs_cache not in {"on", "off"}:
        print("ERROR: --reuse-demucs-cache must be one of: on, off", file=sys.stderr)
        return 2
    reuse_demucs_cache_enabled = reuse_demucs_cache == "on"

    demucs_no_split = str(args.demucs_no_split).strip().lower()
    if demucs_no_split not in {"on", "off"}:
        print("ERROR: --demucs-no-split must be one of: on, off", file=sys.stderr)
        return 2
    demucs_no_split_enabled = demucs_no_split == "on"

    demucs_model = str(args.demucs_model or "htdemucs").strip()
    demucs_shifts = max(1, int(args.demucs_shifts))
    demucs_overlap = _clamp(float(args.demucs_overlap), 0.05, 0.95)
    demucs_device = str(args.demucs_device or "auto").strip().lower() or "auto"
    demucs_segment = float(args.demucs_segment)
    demucs_segment = demucs_segment if demucs_segment > 0.0 else None
    if demucs_no_split_enabled:
        demucs_segment = None
    demucs_jobs = max(0, int(args.demucs_jobs))

    bass_enabled = emit_bass == "on" and "bass" in instruments
    if not bass_enabled:
        bass_stem_mode = "off"
    vocal_enabled = vocal_stem_mode != "off" and "vocal" in instruments

    analysis_input = args.input
    analysis_source = "mix"
    bass_input = args.input
    bass_source = "mix"
    synth_input: Optional[Path] = None
    synth_source = "none"
    vocal_input: Optional[Path] = None
    vocal_source = "none"
    demucs_used = None

    try:
        demucs_cmd: Optional[Sequence[str]] = None
        needs_demucs = stem_mode != "off" or (bass_enabled and bass_stem_mode != "off") or vocal_enabled
        if needs_demucs:
            emit_progress(0.10, "resolving stem separator")
            demucs_cmd = _resolve_demucs_command(args.demucs_bin)
            if demucs_cmd is None:
                if stem_mode == "on":
                    raise RuntimeError(
                        "Drum stem mode is ON but demucs was not found. "
                        "Install demucs or pass --demucs-bin /absolute/path/to/demucs."
                    )
                if bass_enabled and bass_stem_mode == "on":
                    raise RuntimeError(
                        "Bass stem mode is ON but demucs was not found. "
                        "Install demucs or pass --demucs-bin /absolute/path/to/demucs."
                    )
                if vocal_stem_mode == "on":
                    raise RuntimeError(
                        "Vocal stem mode is ON but demucs was not found. "
                        "Install demucs or pass --demucs-bin /absolute/path/to/demucs."
                    )

        if demucs_cmd is not None:
            demucs_used = " ".join(demucs_cmd)
            must_have_demucs_output = stem_mode == "on" or (bass_enabled and bass_stem_mode == "on") or vocal_stem_mode == "on"
            _demucs_tmpdir = None

            synth_enabled = ("synth" in instruments or "pad" in instruments) and stem_mode != "off"
            needs_drum_stem = stem_mode != "off" and "drums" in instruments
            needs_bass_stem = bass_enabled and bass_stem_mode != "off"
            needs_other_stem = (bass_enabled and bass_stem_mode != "off") or synth_enabled
            needs_vocal_stem = vocal_enabled

            cached_stems_root: Optional[Path] = None
            cached_drum_path: Optional[Path] = None
            cached_bass_path: Optional[Path] = None
            cached_other_path: Optional[Path] = None
            cached_vocal_path: Optional[Path] = None
            cache_enabled = args.demucs_cache_dir is not None

            if cache_enabled:
                try:
                    cache_base = args.demucs_cache_dir.expanduser()
                    cache_key = _build_demucs_cache_key(
                        input_path=args.input,
                        demucs_model=demucs_model,
                        demucs_shifts=demucs_shifts,
                        demucs_overlap=demucs_overlap,
                        demucs_device=demucs_device,
                        demucs_segment=demucs_segment,
                        demucs_no_split=demucs_no_split_enabled,
                        demucs_jobs=demucs_jobs,
                    )
                    cache_job_root = cache_base / cache_key
                    cached_stems_root = cache_job_root / "stems"
                    cached_stems_root.mkdir(parents=True, exist_ok=True)
                    cached_drum_path = cached_stems_root / "drums.wav"
                    cached_bass_path = cached_stems_root / "bass.wav"
                    cached_other_path = cached_stems_root / "other.wav"
                    cached_vocal_path = cached_stems_root / "vocals.wav"
                except Exception as cache_exc:
                    print(f"WARN: demucs cache disabled: {cache_exc}", file=sys.stderr)
                    cache_enabled = False
                    cached_stems_root = None
                    cached_drum_path = None
                    cached_bass_path = None
                    cached_other_path = None
                    cached_vocal_path = None

            if cache_enabled:
                drum_cached_ok = (not needs_drum_stem) or (cached_drum_path is not None and cached_drum_path.exists())
                bass_cached_ok = (not needs_bass_stem) or (cached_bass_path is not None and cached_bass_path.exists())
                other_cached_ok = (not needs_other_stem) or (cached_other_path is not None and cached_other_path.exists())
                vocal_cached_ok = (not needs_vocal_stem) or (cached_vocal_path is not None and cached_vocal_path.exists())

                if reuse_demucs_cache_enabled and drum_cached_ok and bass_cached_ok and other_cached_ok and vocal_cached_ok:
                    emit_progress(0.24, "reusing cached demucs stems")
                    if needs_drum_stem and cached_drum_path is not None:
                        analysis_input = cached_drum_path
                        analysis_source = "drum_stem_cache"
                    if needs_bass_stem and cached_bass_path is not None:
                        bass_input = cached_bass_path
                        bass_source = "bass_stem_cache"
                    if needs_other_stem and cached_other_path is not None:
                        synth_input = cached_other_path
                        synth_source = "other_stem_cache"
                    if needs_vocal_stem and cached_vocal_path is not None:
                        vocal_input = cached_vocal_path
                        vocal_source = "vocal_stem_cache"
                else:
                    emit_progress(0.24, "separating stems with demucs")
                    try:
                        if cached_stems_root is not None:
                            demucs_output_root = _separate_all_stems_demucs(
                                input_path=args.input,
                                output_root=cached_stems_root.parent / "raw",
                                demucs_cmd=demucs_cmd,
                                demucs_model=demucs_model,
                                demucs_shifts=demucs_shifts,
                                demucs_overlap=demucs_overlap,
                                demucs_device=demucs_device,
                                demucs_segment=demucs_segment,
                                demucs_no_split=demucs_no_split_enabled,
                                demucs_jobs=demucs_jobs,
                            )
                            emit_progress(0.44, "resolving stems")
                            drum_stem_path, bass_stem_path, other_stem_path, vocal_stem_path = _resolve_requested_demucs_stems(
                                demucs_output_root=demucs_output_root,
                                stem_mode=stem_mode,
                                bass_enabled=bass_enabled,
                                bass_stem_mode=bass_stem_mode,
                                vocal_enabled=vocal_enabled,
                                cached_drum_path=cached_drum_path,
                                cached_bass_path=cached_bass_path,
                                cached_other_path=cached_other_path,
                                cached_vocal_path=cached_vocal_path,
                            )
                            if drum_stem_path is not None:
                                analysis_input = drum_stem_path
                                analysis_source = "drum_stem_cache"
                            if bass_stem_path is not None:
                                bass_input = bass_stem_path
                                bass_source = "bass_stem_cache"
                            if other_stem_path is not None:
                                synth_input = other_stem_path
                                synth_source = "other_stem_cache"
                            if vocal_stem_path is not None:
                                vocal_input = vocal_stem_path
                                vocal_source = "vocal_stem_cache"
                    except Exception as sep_exc:
                        if must_have_demucs_output:
                            raise RuntimeError(f"Demucs stem separation failed: {sep_exc}") from sep_exc
            else:
                print(
                    "TIP: enable --demucs-cache-dir to persist/reuse stems (recommended for build tuning/product).",
                    file=sys.stderr,
                )

                emit_progress(0.24, "separating stems with demucs")
                _demucs_tmpdir = tempfile.mkdtemp(prefix="manifold_demucs_")
                out_root = Path(_demucs_tmpdir)
                try:
                    demucs_output_root = _separate_all_stems_demucs(
                        input_path=args.input,
                        output_root=out_root,
                        demucs_cmd=demucs_cmd,
                        demucs_model=demucs_model,
                        demucs_shifts=demucs_shifts,
                        demucs_overlap=demucs_overlap,
                        demucs_device=demucs_device,
                        demucs_segment=demucs_segment,
                        demucs_no_split=demucs_no_split_enabled,
                        demucs_jobs=demucs_jobs,
                    )
                except Exception as sep_exc:
                    if must_have_demucs_output:
                        raise RuntimeError(f"Demucs stem separation failed: {sep_exc}") from sep_exc
                    demucs_output_root = None

                if demucs_output_root is not None:
                    emit_progress(0.44, "resolving stems")
                    drum_stem_path, bass_stem_path, other_stem_path, vocal_stem_path = _resolve_requested_demucs_stems(
                        demucs_output_root=demucs_output_root,
                        stem_mode=stem_mode,
                        bass_enabled=bass_enabled,
                        bass_stem_mode=bass_stem_mode,
                        vocal_enabled=vocal_enabled,
                        cached_drum_path=None,
                        cached_bass_path=None,
                        cached_other_path=None,
                        cached_vocal_path=None,
                    )
                    if drum_stem_path is not None:
                        analysis_input = drum_stem_path
                        analysis_source = "drum_stem"
                    if bass_stem_path is not None:
                        bass_input = bass_stem_path
                        bass_source = "bass_stem"
                    if other_stem_path is not None:
                        synth_input = other_stem_path
                        synth_source = "other_stem"
                    if vocal_stem_path is not None:
                        vocal_input = vocal_stem_path
                        vocal_source = "vocal_stem"

            if cached_stems_root is not None:
                print(f"Demucs stems cache: {cached_stems_root}")

            emit_progress(0.62, "decoding analysis audio")
            audio, sr = load_audio_mono(analysis_input, target_sr=args.sample_rate, ffmpeg_bin=args.ffmpeg_bin)
            if bass_enabled:
                if str(bass_input) == str(analysis_input):
                    bass_audio = audio
                else:
                    emit_progress(0.66, "decoding bass analysis audio")
                    bass_audio, _ = load_audio_mono(
                        bass_input,
                        target_sr=args.sample_rate,
                        ffmpeg_bin=args.ffmpeg_bin,
                    )
            else:
                bass_audio = None

            if synth_input is not None and ("synth" in instruments or "pad" in instruments):
                if str(synth_input) == str(analysis_input):
                    synth_audio = audio
                elif bass_enabled and str(synth_input) == str(bass_input):
                    synth_audio = bass_audio
                else:
                    emit_progress(0.69, "decoding synth analysis audio")
                    synth_audio, _ = load_audio_mono(
                        synth_input,
                        target_sr=args.sample_rate,
                        ffmpeg_bin=args.ffmpeg_bin,
                    )
            else:
                synth_audio = None

            if vocal_input is not None and "vocal" in instruments:
                emit_progress(0.71, "decoding vocal analysis audio")
                vocal_audio, _ = load_audio_mono(
                    vocal_input,
                    target_sr=args.sample_rate,
                    ffmpeg_bin=args.ffmpeg_bin,
                )
            else:
                vocal_audio = None

            # NOTE: temp demucs directory cleanup is deferred until after
            # analyze_percussion() — madmom onset detection reads stem files
            # from disk.  Cleanup happens after the analysis call below.

        else:
            emit_progress(0.58, "decoding analysis audio")
            audio, sr = load_audio_mono(analysis_input, target_sr=args.sample_rate, ffmpeg_bin=args.ffmpeg_bin)
            bass_audio = audio if bass_enabled else None
            synth_audio = None
            vocal_audio = None

        events, bpm, bpm_confidence, beat_grid, used_profile, bass_profile_used, detection_metrics, energy_envelope = analyze_percussion(
            audio=audio,
            sample_rate=sr,
            frame_size=args.frame_size,
            hop_size=args.hop_size,
            profile_name=args.profile,
            emit_bass=bass_enabled,
            bass_audio=bass_audio,
            synth_audio=synth_audio,
            vocal_audio=vocal_audio,
            bass_profile_name=args.bass_profile,
            bass_sub_weight=args.bass_sub_weight,
            bass_body_weight=args.bass_body_weight,
            bass_bite_weight=args.bass_bite_weight,
            audio_path=str(args.input),
            analysis_audio_path=str(analysis_input),
            bass_audio_path=str(bass_input) if bass_input is not None else None,
            synth_audio_path=str(synth_input) if synth_input is not None else None,
            vocal_audio_path=str(vocal_input) if vocal_input is not None else None,
            min_bpm=args.min_bpm,
            max_bpm=args.max_bpm,
            ffmpeg_bin=args.ffmpeg_bin,
            on_progress=emit_progress,
            detection_config=detection_config,
            instruments=instruments,
        )
    except Exception as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    finally:
        # Clean up temporary demucs directory now that analysis is complete.
        if _demucs_tmpdir is not None:
            shutil.rmtree(_demucs_tmpdir, ignore_errors=True)

    if args.max_events and args.max_events > 0:
        events = events[: args.max_events]

    track_id = args.track_id or args.input.stem
    payload = build_output(
        track_id=track_id,
        bpm=bpm,
        bpm_confidence=bpm_confidence,
        beat_grid=beat_grid,
        events=events,
        energy_envelope=energy_envelope,
    )

    emit_progress(0.90, "writing analysis JSON")
    output_path = args.output or args.input.with_suffix(".percussion.json")
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")

    counts: Dict[str, int] = {}
    for e in events:
        counts[e.type] = counts.get(e.type, 0) + 1

    summary = ", ".join(f"{k}:{v}" for k, v in sorted(counts.items())) or "no events"
    bpm_text = f"{bpm:.2f}" if bpm is not None else "n/a"
    bpm_confidence_text = f"{(100.0 * bpm_confidence):.1f}%"
    emit_progress(0.97, "finalizing analysis summary")
    print(f"Analysis source: {analysis_source}")
    if bass_enabled:
        print(f"Bass source: {bass_source}")
        if synth_source != "none":
            print(f"Synth source: {synth_source}")
    if vocal_source != "none":
        print(f"Vocal source: {vocal_source}")
    if demucs_used is not None:
        print(f"Stem separator: demucs ({demucs_used}) model={args.demucs_model}")
        print(
            "Demucs settings: "
            f"shifts={demucs_shifts}, overlap={demucs_overlap:.3f}, device={demucs_device}, "
            f"segment={('no-split' if demucs_no_split_enabled else ('auto' if demucs_segment is None else f'{demucs_segment:.3f}'))}, "
            f"jobs={demucs_jobs}"
        )
    print(f"Percussion profile: {used_profile}")
    if bass_profile_used is not None:
        print(f"Bass profile: {bass_profile_used}")
    print(f"Wrote {len(events)} events -> {output_path}")
    print(f"Estimated BPM: {bpm_text}")
    print(f"BPM confidence: {bpm_confidence_text}")
    if beat_grid is not None:
        print(
            "Beat grid: "
            f"mode={beat_grid.mode}, beats={len(beat_grid.beat_times)}, "
            f"downbeats={len(beat_grid.downbeat_indices)}, "
            f"confidence={(100.0 * beat_grid.confidence):.1f}%"
        )
    else:
        print("Beat grid: unavailable")
    print(f"Event counts: {summary}")
    print(
        "Detection metrics: "
        f"candidates={detection_metrics.candidate_count}, "
        f"classified={detection_metrics.classified_count}, "
        f"ambiguous={detection_metrics.ambiguous_count}, "
        f"mean_margin={detection_metrics.mean_margin:.4f}, "
        f"kick_snare_overlap={detection_metrics.kick_snare_overlap_rate:.4f}, "
        f"snare_perc_overlap={detection_metrics.snare_perc_overlap_rate:.4f}"
    )
    pre_counts_text = ", ".join(
        f"{k}:{v}" for k, v in sorted(detection_metrics.pre_filter_counts.items())
    ) or "none"
    post_counts_text = ", ".join(
        f"{k}:{v}" for k, v in sorted(detection_metrics.post_filter_counts.items())
    ) or "none"
    print(f"Pre-filter counts: {pre_counts_text}")
    print(f"Post-filter counts: {post_counts_text}")
    emit_progress(1.00, "analysis finished")
    return 0
