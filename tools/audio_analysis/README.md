# Percussion JSON Pipeline

Generate MANIFOLD-compatible percussion trigger JSON from audio files.

## Script

- `Tools/AudioAnalysis/percussion_json_pipeline.py`

## Requirements

- Python 3.9+
- `numpy`
- `ffmpeg` (only needed for non-`.wav` inputs like mp3/aac/flac)
- `demucs` (optional, for drum/bass stem-first analysis)

If ffmpeg is installed but not on PATH for Unity/Python, pass:

```bash
--ffmpeg-bin /absolute/path/to/ffmpeg
```

Install numpy:

```bash
python3 -m pip install numpy
```

## Player Build Packaging (macOS IL2CPP)

For standalone macOS builds, MANIFOLD now prefers a bundled analysis backend:

1. `<App>.app/Contents/Resources/AudioAnalysisRuntime` (bundled runtime)
2. Project-local `Tools/AudioAnalysis/percussion_json_pipeline.py` + local/system Python (Editor/dev fallback)

Build packaging is handled by:

- `Assets/Scripts/Editor/PercussionPipelineBuildPostprocessor.cs`

On macOS player builds, that postprocessor copies:

- top-level `*.py` files from `Tools/AudioAnalysis/` into `AudioAnalysisRuntime`
- optional bundled payload from `Tools/AudioAnalysis/BundledRuntime/macOS/` into `AudioAnalysisRuntime`

The build no longer copies the full project `.venv` as a fallback.
If the bundled runtime has no usable Python backend, the build fails with
an actionable staging command.

Recommended bundled payload layout:

- `.venv/bin/python3` (or `bin/python3`)
- `bin/ffmpeg`
- `bin/demucs` (optional but recommended when drum-stem mode is on)

### Stage Runtime From Editor Environment

Use the staging script to build/update the macOS bundled runtime from one source config:

```bash
Tools/AudioAnalysis/stage_runtime_mac.sh
```

This creates:

- `Tools/AudioAnalysis/BundledRuntime/macOS/.venv`
- `Tools/AudioAnalysis/BundledRuntime/macOS/bin/ffmpeg` (if found)
- `Tools/AudioAnalysis/BundledRuntime/macOS/percussion_json_pipeline.py`
- `Tools/AudioAnalysis/BundledRuntime/macOS/lameenc.py`

The script installs dependencies from:

- `Tools/AudioAnalysis/requirements.runtime.mac.txt`

Show script options:

```bash
Tools/AudioAnalysis/stage_runtime_mac.sh --help
```

## Basic Usage

```bash
python3 Tools/AudioAnalysis/percussion_json_pipeline.py \
  /path/to/track.wav \
  -o Debugging/track.percussion.json
```

For mp3/aac/flac input (requires ffmpeg):

```bash
python3 Tools/AudioAnalysis/percussion_json_pipeline.py \
  /path/to/track.mp3 \
  -o Debugging/track.percussion.json
```

Then in MANIFOLD, click `PERC` (or `Cmd/Ctrl+Shift+I`) and import the generated JSON.

## Import Progress Signaling

`percussion_json_pipeline.py` now emits progress markers that the Timeline `PERC` import UI
can parse for stage-level status updates and a determinate progress bar:

```text
MANIFOLD_PROGRESS|0.740|detecting percussion events
```

Marker format:

- Prefix: `MANIFOLD_PROGRESS|`
- Field 1: normalized progress `0.0 .. 1.0`
- Field 2: short stage message for UI display

## Kick/Snare Deconflict

The detector now applies conflict resolution before JSON emit:

- Snare candidates near strong kick transients are suppressed unless there is clear
  snare/high-band confirmation.
- Snare/perc overlaps are also disambiguated so ambiguous transients prefer a single label.
- This reduces mirrored `kick` + `snare` events and double-tagged `snare`/`perc` hits.

## Class-Scored Drum Detection (Single Profile)

Drum detection now uses a single-profile scored classifier (still deterministic, no ML):

- high-recall transient candidate stage
- per-candidate timbre/envelope/context feature scoring for `kick`, `snare`, `hat`, `perc`, `clap`
- track-local prototype memory bonus for class consistency (for example "this song's snare")
- post-score overlap and spacing arbitration (`snare`/`perc`, class-specific spacing)

Analysis summary output now includes detector observability fields:

- `Detection metrics: candidates=..., classified=..., ambiguous=..., ...`
- `Pre-filter counts: ...`
- `Post-filter counts: ...`

## Useful Flags

- `--track-id "MyTrack"` override track ID in JSON
- `--use-drum-stem auto|on|off` try Demucs and analyze drum stem (`auto` recommended)
- `--emit-bass on|off` enable bass gesture events (`off` default for script compatibility)
- `--use-bass-stem auto|on|off` try Demucs and analyze bass stem (`auto` recommended when bass enabled)
- When drum and bass stem modes are enabled, Demucs is now run once and both stems are reused from that single separation pass.
- `--demucs-cache-dir /absolute/path` persist stems for reuse/auditioning (prints `Demucs stems cache: ...`)
- `--reuse-demucs-cache on|off` reuse persisted stems if present (`on` default)
- `--demucs-bin /absolute/path/to/demucs` custom Demucs binary path
- `--demucs-model htdemucs` model to load for stem separation
- `--profile electronic` drum detection profile (single-profile mode)
- `--bass-profile electronic` bass detection profile (single-profile mode)
- `--min-confidence 0.50` remove weaker events
- `--threshold-scale 1.15` stricter detection (fewer events)
- `--threshold-scale 0.85` more sensitive detection (more events)
- `--bass-min-confidence 0.50` remove weaker bass gesture events
- `--bass-threshold-scale 1.10` stricter bass detection
- `--bass-sub-weight / --bass-body-weight / --bass-bite-weight` override bass gesture weighting
- `--max-events 1000` cap total events

Example with tuning:

```bash
python3 Tools/AudioAnalysis/percussion_json_pipeline.py \
  /path/to/track.wav \
  -o Debugging/track.percussion.json \
  --track-id "Club Set 01" \
  --profile electronic \
  --emit-bass on \
  --use-bass-stem auto \
  --min-confidence 0.5 \
  --threshold-scale 1.05
```

Default MANIFOLD import invocation currently prefers drum-stem analysis and uses:

- `--use-drum-stem on`
- `--min-confidence 0.38`
- `--threshold-scale 0.96`

## Output Format

The script writes:

```json
{
  "trackId": "track",
  "bpm": 127.84,
  "bpmConfidence": 0.83,
  "beatGrid": {
    "mode": "librosa",
    "bpmDerived": 127.84,
    "confidence": 0.83,
    "beatTimes": [0.0000, 0.4690, 0.9381, 1.4072],
    "downbeatIndices": [0]
  },
  "events": [
    { "type": "kick", "time": 0.5000, "confidence": 0.9211 },
    { "type": "snare", "time": 1.0000, "confidence": 0.8760 },
    { "type": "hat", "time": 1.2500, "confidence": 0.7440 },
    { "type": "bass", "time": 1.5000, "confidence": 0.7012 }
  ]
}
```

Event labels emitted by the pipeline: `kick`, `snare`, `clap`, `hat`, `perc`, `bass`, `synth` (for stem-enabled runs).
The current bass path is single-lane (bass stem only), and `synth` events are emitted from the `other` stem when available.
`bpm` is derived from beat-grid intervals when grid extraction succeeds.

## Build Reminder

For build/runtime tuning, explicitly enable cached stems in backend invocation:

- set `MANIFOLD_DEMUCS_CACHE=1`
- optionally set `MANIFOLD_DEMUCS_CACHE_DIR=/absolute/cache/path`

Without these, build invocations default to no persisted stem cache.
