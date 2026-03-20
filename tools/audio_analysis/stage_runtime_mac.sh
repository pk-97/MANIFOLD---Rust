#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TOOLS_DIR="$PROJECT_ROOT/Tools/AudioAnalysis"

SOURCE_VENV_DEFAULT="$TOOLS_DIR/.venv"
TARGET_DEFAULT="$TOOLS_DIR/BundledRuntime/macOS"
REQUIREMENTS_DEFAULT="$TOOLS_DIR/requirements.runtime.mac.txt"
CACHE_DEFAULT="$TOOLS_DIR/BundledRuntime/.cache"

# python-build-standalone release to download.
PBS_RELEASE_TAG="20260211"
PBS_CPYTHON_VERSION="3.12.12"
PBS_BASE_URL="https://github.com/astral-sh/python-build-standalone/releases/download/${PBS_RELEASE_TAG}"

SOURCE_VENV="$SOURCE_VENV_DEFAULT"
TARGET_ROOT="$TARGET_DEFAULT"
REQUIREMENTS_FILE="$REQUIREMENTS_DEFAULT"
CACHE_DIR="$CACHE_DEFAULT"
SKIP_INSTALL=0
FORCE=0

usage() {
  cat <<USAGE
Usage: $(basename "$0") [options]

Stage a fully portable macOS analysis runtime for player builds.
Downloads a self-contained Python (python-build-standalone) so the
built app works on any Mac without system Python.

Options:
  --source-venv <path>    Source editor venv (used to locate ffmpeg)
  --target <path>         Bundled runtime output root (default: $TARGET_DEFAULT)
  --requirements <path>   pip requirements file (default: $REQUIREMENTS_DEFAULT)
  --cache <path>          Download cache directory (default: $CACHE_DEFAULT)
  --skip-install          Recreate layout and scripts, skip pip install
  --force                 Force full restage even if runtime appears valid
  -h, --help              Show this help
USAGE
}

log() {
  printf '[stage-runtime] %s\n' "$*"
}

err() {
  printf '[stage-runtime] ERROR: %s\n' "$*" >&2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source-venv)  SOURCE_VENV="$2";       shift 2 ;;
    --target)       TARGET_ROOT="$2";        shift 2 ;;
    --requirements) REQUIREMENTS_FILE="$2";  shift 2 ;;
    --cache)        CACHE_DIR="$2";          shift 2 ;;
    --skip-install) SKIP_INSTALL=1;          shift   ;;
    --force)        FORCE=1;                 shift   ;;
    -h|--help)      usage; exit 0            ;;
    *)              err "Unknown option: $1"; usage; exit 2 ;;
  esac
done

if [[ "$SKIP_INSTALL" -eq 0 && ! -f "$REQUIREMENTS_FILE" ]]; then
  err "Requirements file not found: $REQUIREMENTS_FILE"
  exit 2
fi

PIPELINE_SCRIPT="$TOOLS_DIR/percussion_json_pipeline.py"
SHIM_SCRIPT="$TOOLS_DIR/lameenc.py"

if [[ ! -f "$PIPELINE_SCRIPT" || ! -f "$SHIM_SCRIPT" ]]; then
  err "Expected pipeline scripts missing in $TOOLS_DIR"
  exit 2
fi

TARGET_ROOT="$(cd "$(dirname "$TARGET_ROOT")" && pwd)/$(basename "$TARGET_ROOT")"
mkdir -p "$TARGET_ROOT"

PYTHON_DIR="$TARGET_ROOT/python"
BIN_DIR="$TARGET_ROOT/bin"

# ── Helper: refresh pipeline scripts ────────────────────────────────────

refresh_scripts() {
  log "Refreshing MANIFOLD pipeline scripts"
  cp "$PIPELINE_SCRIPT" "$TARGET_ROOT/percussion_json_pipeline.py"
  cp "$SHIM_SCRIPT"     "$TARGET_ROOT/lameenc.py"
  chmod +x "$TARGET_ROOT/percussion_json_pipeline.py"

  if [[ -d "$TOOLS_DIR/manifold_audio" ]]; then
    rm -rf "$TARGET_ROOT/manifold_audio"
    cp -R "$TOOLS_DIR/manifold_audio" "$TARGET_ROOT/manifold_audio"
    find "$TARGET_ROOT/manifold_audio" -name '__pycache__' -type d -exec rm -rf {} + 2>/dev/null || true
  fi
}

# ── Helper: fix shebangs to use env for portability ─────────────────────

fix_shebangs() {
  local target_bin_dir="$1"
  log "Fixing shebangs in $target_bin_dir"

  for f in "$target_bin_dir"/*; do
    [[ -f "$f" && -x "$f" ]] || continue
    local first_line
    first_line="$(head -1 "$f")" || continue
    case "$first_line" in
      "#!"*python*)
        sed -i '' '1s|^#!.*python[0-9.]*|#!/usr/bin/env python3|' "$f"
        ;;
    esac
  done
}

# ── Skip-when-valid check ──────────────────────────────────────────────

runtime_is_valid() {
  local py="$PYTHON_DIR/bin/python3"
  [[ -x "$py" ]] || return 1

  local ver
  ver="$("$py" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")' 2>/dev/null)" || return 1
  local major="${ver%%.*}"
  local minor="${ver##*.}"
  [[ "$major" -eq 3 && "$minor" -ge 10 && "$minor" -le 13 ]] || return 1

  "$py" -c "import numpy, demucs, madmom, adtof_pytorch, basic_pitch" 2>/dev/null || return 1

  local req_hash
  req_hash="$(shasum -a 256 "$REQUIREMENTS_FILE" 2>/dev/null | cut -d' ' -f1)" || return 1
  local stored_hash=""
  local manifest="$TARGET_ROOT/runtime-manifest.txt"
  if [[ -f "$manifest" ]]; then
    stored_hash="$(grep '^RequirementsHash:' "$manifest" 2>/dev/null | cut -d' ' -f2)" || true
  fi
  [[ "$req_hash" == "$stored_hash" ]] || return 1

  return 0
}

if [[ "$SKIP_INSTALL" -eq 1 ]]; then
  log "Refreshing scripts only (--skip-install)"
  refresh_scripts
  log "Done (skipped pip install and Python extraction)."
  exit 0
fi

if [[ "$FORCE" -eq 0 ]] && runtime_is_valid; then
  log "Bundled runtime is already valid — refreshing scripts only"
  refresh_scripts
  log "Done (skipped pip install). Use --force to restage from scratch."
  exit 0
fi

# ── Detect architecture ────────────────────────────────────────────────

MACHINE="$(uname -m)"
case "$MACHINE" in
  arm64)  PBS_ARCH="aarch64" ;;
  x86_64) PBS_ARCH="x86_64"  ;;
  *)
    err "Unsupported architecture: $MACHINE"
    exit 2
    ;;
esac

PBS_FILENAME="cpython-${PBS_CPYTHON_VERSION}+${PBS_RELEASE_TAG}-${PBS_ARCH}-apple-darwin-install_only_stripped.tar.gz"
PBS_URL="${PBS_BASE_URL}/${PBS_FILENAME}"

# ── Download python-build-standalone ───────────────────────────────────

mkdir -p "$CACHE_DIR"
CACHED_TARBALL="$CACHE_DIR/$PBS_FILENAME"

if [[ -f "$CACHED_TARBALL" ]]; then
  log "Using cached Python tarball: $CACHED_TARBALL"
else
  log "Downloading Python standalone ($PBS_FILENAME)..."
  if ! curl -fSL --progress-bar -o "$CACHED_TARBALL.tmp" "$PBS_URL"; then
    rm -f "$CACHED_TARBALL.tmp"
    err "Failed to download: $PBS_URL"
    exit 1
  fi
  mv "$CACHED_TARBALL.tmp" "$CACHED_TARBALL"
  log "Downloaded to cache: $CACHED_TARBALL"
fi

# ── Extract to target ──────────────────────────────────────────────────

if [[ -d "$PYTHON_DIR" ]]; then
  log "Removing existing Python directory"
  rm -rf "$PYTHON_DIR"
fi

log "Extracting Python to $PYTHON_DIR"
mkdir -p "$PYTHON_DIR"
# The tarball extracts to a top-level python/ directory.
tar -xzf "$CACHED_TARBALL" -C "$TARGET_ROOT"

BUNDLED_PY="$PYTHON_DIR/bin/python3"
if [[ ! -x "$BUNDLED_PY" ]]; then
  err "Python binary missing after extraction: $BUNDLED_PY"
  exit 1
fi

PY_VER="$("$BUNDLED_PY" -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')"
log "Standalone Python $PY_VER ready"

# ── Install packages ───────────────────────────────────────────────────

mkdir -p "$BIN_DIR"

log "Installing build dependencies (numpy, cython, setuptools)"
"$BUNDLED_PY" -m pip install --upgrade pip setuptools wheel cython numpy

log "Installing runtime dependencies from $REQUIREMENTS_FILE"
# Phase 1: Install madmom with --no-build-isolation (it needs numpy + Cython
# at build time but doesn't declare them). Must be separate because
# --no-build-isolation breaks metadata generation for packages like soxr.
log "Phase 1/3: installing madmom (--no-build-isolation)"
"$BUNDLED_PY" -m pip install --no-build-isolation \
  "madmom @ git+https://github.com/CPJKU/madmom.git@main"

# Phase 2: Install basic-pitch with --no-deps. Its metadata pulls
# tensorflow-macos<2.15.1 on macOS+Python3.12 which has no wheels.
# basic-pitch works with coremltools as its sole backend on macOS.
log "Phase 2/3: installing basic-pitch (--no-deps) + coremltools"
"$BUNDLED_PY" -m pip install --no-deps basic-pitch
"$BUNDLED_PY" -m pip install coremltools

# Phase 3: Install everything else normally (pip uses binary wheels).
# Filter out madmom and basic-pitch (already installed in phases 1-2)
# to prevent pip from re-resolving their problematic dep trees.
log "Phase 3/3: installing remaining dependencies"
grep -v -E '^(madmom|basic-pitch)' "$REQUIREMENTS_FILE" | \
  "$BUNDLED_PY" -m pip install -r /dev/stdin

# Phase 3b: Install basic-pitch's actual runtime deps (minus tensorflow).
# basic-pitch uses CoreML on macOS — tensorflow is not needed.
log "Installing basic-pitch runtime deps (no tensorflow)"
"$BUNDLED_PY" -m pip install librosa "mir-eval>=0.6" "resampy>=0.2.2,<0.4.3" scikit-learn typing-extensions

fix_shebangs "$PYTHON_DIR/bin"

# ── Copy pipeline scripts ──────────────────────────────────────────────

refresh_scripts

# ── Copy ffmpeg ────────────────────────────────────────────────────────

resolve_ffmpeg() {
  if [[ -n "${FFMPEG_PATH:-}" && -x "$FFMPEG_PATH" ]]; then
    printf '%s' "$FFMPEG_PATH"
    return
  fi

  if command -v ffmpeg >/dev/null 2>&1; then
    command -v ffmpeg
    return
  fi

  if [[ -x "$SOURCE_VENV/bin/ffmpeg" ]]; then
    printf '%s' "$SOURCE_VENV/bin/ffmpeg"
    return
  fi
}

FFMPEG_BIN="$(resolve_ffmpeg || true)"
if [[ -n "$FFMPEG_BIN" && -x "$FFMPEG_BIN" ]]; then
  log "Copying ffmpeg from $FFMPEG_BIN"
  cp "$FFMPEG_BIN" "$BIN_DIR/ffmpeg"
  chmod +x "$BIN_DIR/ffmpeg"

  # Also copy ffprobe (sibling of ffmpeg) for MP3 encoder delay probing
  FFPROBE_BIN="${FFMPEG_BIN%/*}/ffprobe"
  if [[ -x "$FFPROBE_BIN" ]]; then
    log "Copying ffprobe from $FFPROBE_BIN"
    cp "$FFPROBE_BIN" "$BIN_DIR/ffprobe"
    chmod +x "$BIN_DIR/ffprobe"
  else
    log "ffprobe not found alongside ffmpeg; MP3 encoder delay probing unavailable in builds"
  fi
else
  log "ffmpeg not found; runtime can still process .wav but not compressed inputs"
fi

# ── Write manifest ─────────────────────────────────────────────────────

REQ_HASH="$(shasum -a 256 "$REQUIREMENTS_FILE" 2>/dev/null | cut -d' ' -f1)"

MANIFEST="$TARGET_ROOT/runtime-manifest.txt"
{
  echo "Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "Python: $("$BUNDLED_PY" --version 2>&1)"
  echo "Source: python-build-standalone $PBS_RELEASE_TAG ($PBS_ARCH)"
  echo "Requirements: $REQUIREMENTS_FILE"
  echo "RequirementsHash: $REQ_HASH"
  echo "Packages:"
  "$BUNDLED_PY" -m pip show numpy librosa demucs madmom adtof-pytorch basic-pitch pretty-midi 2>/dev/null | awk '/^Name:|^Version:/{print "  "$0}'
  if [[ -x "$BIN_DIR/ffmpeg" ]]; then
    echo "ffmpeg: $BIN_DIR/ffmpeg"
  else
    echo "ffmpeg: not bundled"
  fi
  if [[ -x "$BIN_DIR/ffprobe" ]]; then
    echo "ffprobe: $BIN_DIR/ffprobe"
  else
    echo "ffprobe: not bundled"
  fi
} > "$MANIFEST"

log "Staged runtime successfully"
log "Manifest: $MANIFEST"
log "Next build will package this folder into <App>.app/Contents/Resources/AudioAnalysisRuntime"
