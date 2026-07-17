"""P2 listen-list: renders a click at each Beat This-detected beat, mixed
over the original audio, so Peter can listen and judge grid quality by ear
(AUDIO_ANALYSIS_ACCURACY_DESIGN.md P2 deliverable — 10 tracks, beat click
renders). Downbeats get a louder/lower click so the bar structure is
audible too. Output is gitignored (eval/data/ — see eval/.gitignore).

Usage: python -m eval.render_beat_clicks --out-dir eval/data/listen_list
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np
import soundfile as sf

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from manifold_audio.analyzer import analyze_percussion  # noqa: E402
from manifold_audio.audio_io import load_audio_mono  # noqa: E402

SAMPLE_RATE = 44100


def _click(sr: int, freq: float, dur_sec: float = 0.03, decay_sec: float = 0.012) -> np.ndarray:
    n = int(round(dur_sec * sr))
    t = np.arange(n) / sr
    env = np.exp(-t / decay_sec)
    tone = np.sin(2 * np.pi * freq * t)
    return (tone * env).astype(np.float32)


def render_click_track_over_audio(
    audio: np.ndarray,
    sr: int,
    beat_times_sec: List[float],
    downbeat_times_sec: List[float],
    click_gain: float = 0.35,
) -> np.ndarray:
    """Mono in, stereo out: original audio in both channels, clicks summed
    into the RIGHT channel only so Peter can pan to isolate them if useful."""
    beat_click = _click(sr, freq=1800.0)
    downbeat_click = _click(sr, freq=900.0, dur_sec=0.05, decay_sec=0.02)

    click_track = np.zeros_like(audio, dtype=np.float32)
    downbeat_set = {round(t, 3) for t in downbeat_times_sec}
    for t in beat_times_sec:
        is_downbeat = round(t, 3) in downbeat_set
        click = downbeat_click if is_downbeat else beat_click
        start = int(round(t * sr))
        end = min(len(click_track), start + len(click))
        if start >= len(click_track):
            continue
        click_track[start:end] += click[: end - start]

    peak = float(np.max(np.abs(click_track))) if click_track.size else 0.0
    if peak > 0:
        click_track = click_track / peak * click_gain

    left = audio.astype(np.float32)
    right = np.clip(audio.astype(np.float32) * 0.8 + click_track, -1.0, 1.0)
    return np.stack([left, right], axis=1)


def render_one(audio_path: Path, out_path: Path, label: str) -> Tuple[Optional[float], str, int, int]:
    audio, sr = load_audio_mono(audio_path, target_sr=SAMPLE_RATE, ffmpeg_bin=None)
    events, bpm, bpm_conf, beat_grid, *_ = analyze_percussion(
        audio=audio,
        sample_rate=sr,
        frame_size=1024,
        hop_size=256,
        profile_name="electronic",
        emit_bass=False,
        audio_path=str(audio_path),
        analysis_audio_path=str(audio_path),
        min_bpm=55.0,
        max_bpm=215.0,
        instruments=frozenset({"drums"}),
    )
    if beat_grid is None or not beat_grid.beat_times:
        print(f"[{label}] no beat grid — skipping render")
        return bpm, "none", 0, 0

    downbeat_times = [beat_grid.beat_times[i] for i in beat_grid.downbeat_indices if 0 <= i < len(beat_grid.beat_times)]
    stereo = render_click_track_over_audio(audio, sr, beat_grid.beat_times, downbeat_times)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    sf.write(str(out_path), stereo, sr, subtype="PCM_16")
    print(
        f"[{label}] bpm={bpm:.2f} tracker={beat_grid.tracker} "
        f"beats={len(beat_grid.beat_times)} downbeats={len(downbeat_times)} -> {out_path}"
    )
    return bpm, beat_grid.tracker, len(beat_grid.beat_times), len(downbeat_times)


def main(argv: Optional[List[str]] = None) -> int:
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", type=Path, default=Path("eval/data/listen_list"))
    args = parser.parse_args(argv)

    repo_root = Path(__file__).resolve().parents[3]
    own_stems_root = repo_root / "tests" / "fixtures" / "audio"
    liveshow_slices = Path("eval/data/liveshow_song_slices")
    babyslakh_root = Path("eval/data/babyslakh_16k/babyslakh_16k")

    tracks: List[Tuple[str, Path]] = []
    for name in ["apricots_128bpm", "bad_guy_128bpm", "feel_the_vibration_174bpm", "inhale_exhale_145bpm", "tears_140bpm"]:
        tracks.append((name, own_stems_root / name / "mix.wav"))
    for name in ["liveshow_pattern", "liveshow_basalt", "liveshow_all_in_for_you"]:
        tracks.append((name, liveshow_slices / f"{name}.wav"))
    for track_dir in sorted(babyslakh_root.glob("Track*"))[:2]:
        tracks.append((f"babyslakh_{track_dir.name}", track_dir / "mix.wav"))

    for label, audio_path in tracks:
        if not audio_path.exists():
            print(f"[{label}] missing {audio_path} — skipping")
            continue
        out_path = args.out_dir / f"{label}.wav"
        render_one(audio_path, out_path, label)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
