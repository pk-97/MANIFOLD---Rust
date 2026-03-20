"""External binary resolution (ffmpeg, demucs) and stem separation orchestration."""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

from manifold_audio.math_utils import _clamp


def _probe_binary(path: str) -> bool:
    if not path:
        return False
    try:
        proc = subprocess.run(
            [path, "-version"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=3,
            check=False,
        )
        return proc.returncode == 0
    except Exception:
        return False


def _resolve_ffmpeg_path(explicit_path: Optional[str] = None) -> Optional[str]:
    candidates: List[str] = []
    seen: set[str] = set()

    def add(path: Optional[str]) -> None:
        if not path:
            return
        p = path.strip()
        if not p or p in seen:
            return
        seen.add(p)
        candidates.append(p)

    add(explicit_path)
    add(os.environ.get("FFMPEG_PATH"))
    add(shutil.which("ffmpeg"))
    add("/opt/homebrew/bin/ffmpeg")
    add("/usr/local/bin/ffmpeg")
    add("/usr/bin/ffmpeg")

    # Finder-launched Unity often has a minimal PATH; query login shell as fallback.
    try:
        resolved = subprocess.run(
            ["/bin/sh", "-lc", "command -v ffmpeg"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            timeout=3,
            check=False,
        )
        add((resolved.stdout or "").strip())
    except Exception:
        pass

    for candidate in candidates:
        if _probe_binary(candidate):
            return candidate

    return None


def _probe_command(prefix: Sequence[str], timeout_sec: float = 8.0) -> bool:
    if not prefix:
        return False
    try:
        proc = subprocess.run(
            list(prefix) + ["--help"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=timeout_sec,
            check=False,
            env=_build_subprocess_env_with_shims(),
        )
        return proc.returncode == 0
    except Exception:
        return False


def _resolve_demucs_command(explicit_path: Optional[str] = None) -> Optional[List[str]]:
    candidates: List[List[str]] = []
    seen: set[str] = set()

    def add_path(path: Optional[str]) -> None:
        if not path:
            return
        p = path.strip()
        if not p or p in seen:
            return
        seen.add(p)
        candidates.append([p])

    add_path(explicit_path)
    add_path(os.environ.get("DEMUCS_PATH"))
    add_path(shutil.which("demucs"))
    add_path("/opt/homebrew/bin/demucs")
    add_path("/usr/local/bin/demucs")
    add_path("/usr/bin/demucs")

    # Module fallback (works when demucs is installed in active Python env only).
    candidates.append([sys.executable, "-m", "demucs.separate"])

    for cmd in candidates:
        if _probe_command(cmd):
            return cmd

    return None


def _build_subprocess_env_with_shims() -> Dict[str, str]:
    env = dict(os.environ)
    shim_dir = str(Path(__file__).resolve().parent.parent)
    current = env.get("PYTHONPATH", "")
    if current:
        env["PYTHONPATH"] = shim_dir + os.pathsep + current
    else:
        env["PYTHONPATH"] = shim_dir
    return env


def _find_named_stem_file(root: Path, stem_name: str) -> Optional[Path]:
    if not root.exists():
        return None

    stem_key = (stem_name or "").strip().lower()
    if not stem_key:
        return None

    exact = list(root.rglob(f"{stem_key}.wav"))
    if exact:
        return exact[0]

    for p in root.rglob("*"):
        if not p.is_file():
            continue
        name = p.name.lower()
        if stem_key in name and f"no_{stem_key}" not in name:
            return p
    return None


def _separate_all_stems_demucs(
    input_path: Path,
    output_root: Path,
    demucs_cmd: Sequence[str],
    demucs_model: str,
    demucs_shifts: int,
    demucs_overlap: float,
    demucs_device: str,
    demucs_segment: Optional[float],
    demucs_no_split: bool,
    demucs_jobs: int,
) -> Path:
    base_cmd = list(demucs_cmd) + [
        "-n",
        demucs_model,
    ]

    shifts = max(1, int(demucs_shifts))
    if shifts > 1:
        base_cmd.extend(["--shifts", str(shifts)])

    overlap = _clamp(float(demucs_overlap), 0.05, 0.95)
    base_cmd.extend(["--overlap", f"{overlap:.3f}"])

    jobs = max(0, int(demucs_jobs))
    if jobs > 0:
        base_cmd.extend(["-j", str(jobs)])

    if demucs_no_split:
        base_cmd.append("--no-split")
    elif demucs_segment is not None and demucs_segment > 0.0:
        base_cmd.extend(["--segment", f"{float(demucs_segment):.3f}"])

    device_raw = str(demucs_device or "").strip().lower()
    if device_raw in {"", "auto"}:
        if sys.platform == "darwin":
            device_candidates = ["mps", "cpu"]
        else:
            device_candidates = ["cuda", "cpu"]
    else:
        device_candidates = [device_raw]

    last_exc: Optional[Exception] = None
    for i, device in enumerate(device_candidates):
        cmd = list(base_cmd) + [
            "-d",
            device,
            "-o",
            str(output_root),
            str(input_path),
        ]
        try:
            if i > 0:
                print(f"WARN: demucs device fallback -> {device}", file=sys.stderr)
            subprocess.run(cmd, check=True, env=_build_subprocess_env_with_shims())
            return output_root
        except Exception as exc:
            last_exc = exc
            continue

    if last_exc is not None:
        raise last_exc
    return output_root


def _build_demucs_cache_key(
    input_path: Path,
    demucs_model: str,
    demucs_shifts: int,
    demucs_overlap: float,
    demucs_device: str,
    demucs_segment: Optional[float],
    demucs_no_split: bool,
    demucs_jobs: int,
) -> str:
    try:
        stat = input_path.stat()
        size = stat.st_size
        mtime = stat.st_mtime_ns
    except Exception:
        size = -1
        mtime = -1

    model = str(demucs_model or "").strip().lower()
    device = str(demucs_device or "").strip().lower()
    overlap = _clamp(float(demucs_overlap), 0.05, 0.95)
    segment_text = f"{float(demucs_segment):.3f}" if demucs_segment is not None and demucs_segment > 0.0 else "none"
    no_split = "1" if bool(demucs_no_split) else "0"
    shifts = max(1, int(demucs_shifts))
    jobs = max(0, int(demucs_jobs))
    raw = (
        f"{input_path.resolve()}|{size}|{mtime}|model={model}|shifts={shifts}|overlap={overlap:.3f}|"
        f"device={device}|segment={segment_text}|nosplit={no_split}|jobs={jobs}"
    )
    return hashlib.sha1(raw.encode("utf-8")).hexdigest()[:16]


def _cache_stem_file(src: Optional[Path], dst: Optional[Path]) -> Optional[Path]:
    if src is None:
        return None
    if dst is None:
        return src

    try:
        dst.parent.mkdir(parents=True, exist_ok=True)
        if not dst.exists():
            shutil.copy2(src, dst)
        return dst
    except Exception:
        # Best effort: fall back to source stem path.
        return src


def _resolve_requested_demucs_stems(
    demucs_output_root: Path,
    stem_mode: str,
    bass_enabled: bool,
    bass_stem_mode: str,
    vocal_enabled: bool,
    cached_drum_path: Optional[Path],
    cached_bass_path: Optional[Path],
    cached_other_path: Optional[Path],
    cached_vocal_path: Optional[Path] = None,
) -> Tuple[Optional[Path], Optional[Path], Optional[Path], Optional[Path]]:
    drum_stem_path: Optional[Path] = None
    bass_stem_path: Optional[Path] = None
    other_stem_path: Optional[Path] = None
    vocal_stem_path: Optional[Path] = None

    if stem_mode != "off":
        drum_stem = _find_named_stem_file(demucs_output_root, "drums")
        if drum_stem is None and stem_mode == "on":
            raise RuntimeError("Demucs finished but no drum stem file was produced.")
        drum_stem_path = _cache_stem_file(drum_stem, cached_drum_path)

    if bass_enabled and bass_stem_mode != "off":
        bass_stem = _find_named_stem_file(demucs_output_root, "bass")
        other_stem = _find_named_stem_file(demucs_output_root, "other")
        if bass_stem is None and bass_stem_mode == "on":
            raise RuntimeError("Demucs finished but no bass stem file was produced.")
        bass_stem_path = _cache_stem_file(bass_stem, cached_bass_path)
        other_stem_path = _cache_stem_file(other_stem, cached_other_path)

    if vocal_enabled:
        vocal_stem = _find_named_stem_file(demucs_output_root, "vocals")
        vocal_stem_path = _cache_stem_file(vocal_stem, cached_vocal_path)

    return drum_stem_path, bass_stem_path, other_stem_path, vocal_stem_path
