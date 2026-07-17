"""Live-show eval fixture extractor (Addendum 2026-07-17).

Reads a `.manifold` project (a ZIP: project.json + manifest.json) and emits
harness labels of three types, per the addendum:

    grid truth     — the project's tempoMap (the set is grid-locked)
    onset truth    — clip starts in drum-built sections, for layers whose
                     layer->instrument affinity (excess) is >= a threshold
    section truth  — clip starts in synth/ambient sections (placed ahead of
                     swells, quantized — NOT acoustic onsets), for layers
                     with ~zero affinity to any instrument class

This tool reads ONLY the project (grid + per-layer clip edges) directly —
no Rust dependency, no risk to the project file (read-only, no
project_tool-style write path needed). The onset-vs-section classification
additionally needs a layer->instrument affinity file (edge_vs_app_events.json
shape: a list of {layer, inst, recall, null, excess, offset_ms}), which is a
DERIVED product of running the current pipeline against every layer's edges
— not something this tool re-derives itself (that measurement already ran
this session; --affinity accepts its output). Without --affinity, this tool
still emits grid truth + raw per-layer edges, just unclassified.

This is NOT the same thing as tests/fixtures/audio_labels (kick-only CSVs
graded by the Rust-side mod_harness, docs/AUDIO_EVAL_HARNESS_GUIDE.md) — that
harness is real-time/causal detector grading; this one feeds the offline
eval/ package.

Usage:
    python -m eval.liveshow_extract \
        --project "/path/to/Liveschool Live Show V6 AUDIO.manifold" \
        --affinity "/path/to/edge_vs_app_events.json" \
        --out-dir eval/liveshow_labels

Verified against the real project 2026-07-17: layer.edges_beats ==
sorted(clip.startBeat for clip in layer.clips); tempoMap.points matches the
98-point ground-truth tempo map 1:1 (minus the -1.0s sentinel first point).
"""

from __future__ import annotations

import argparse
import json
import zipfile
from collections import defaultdict
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Dict, List, Optional, Tuple

ONSET_EXCESS_THRESHOLD = 0.2
SECTION_EXCESS_CEILING = 0.05  # "excess ~= 0" per the addendum


def load_project_json(manifold_path: Path) -> dict:
    with zipfile.ZipFile(manifold_path) as zf:
        with zf.open("project.json") as f:
            return json.load(f)


@dataclass(frozen=True)
class TempoPoint:
    beat: float
    bpm: float
    recorded_at_seconds: Optional[float]
    source: int


def extract_tempo_map(project: dict) -> List[TempoPoint]:
    """Excludes the -1.0s sentinel point (pre-recording placeholder) — same
    filter tempo_points.json (the pre-extracted reference) already applied."""
    points = project["tempoMap"]["points"]
    out = []
    for p in points:
        recorded = p.get("recordedAtSeconds")
        if recorded is not None and recorded < 0:
            continue
        out.append(TempoPoint(beat=p["beat"], bpm=p["bpm"], recorded_at_seconds=recorded, source=p.get("source", -1)))
    return out


@dataclass(frozen=True)
class LayerEdges:
    name: str
    index: int
    edges_beats: List[float]
    durations_beats: List[float]


_LAYER_TYPE_PERFORMABLE = 1  # verified against the real project 2026-07-17:
# layerType is an int enum. 1 = ordinary performable video/generator layers
# (46 of them in the reference project, matching edge_corpus.json exactly);
# 2 = song-group container layers (zero clips each — organizational only,
# not performable objects); 3 = the single master-audio layer (the audio
# clip itself, not a triggerable object). Only 1 belongs in edge/onset/
# section truth.


def extract_layer_edges(project: dict) -> List[LayerEdges]:
    """edges_beats = sorted clip startBeat per layer (verified against the
    project 2026-07-17: CLAP layer, index=2, n=224 clips, matches
    edge_corpus.json's layers[0] exactly)."""
    out = []
    for layer in project["timeline"]["layers"]:
        if layer.get("layerType") != _LAYER_TYPE_PERFORMABLE:
            continue
        clips = layer.get("clips") or []
        if not clips:
            continue
        paired = sorted((c["startBeat"], c.get("durationBeats", 0.0)) for c in clips)
        out.append(
            LayerEdges(
                name=layer["name"],
                index=layer["index"],
                edges_beats=[p[0] for p in paired],
                durations_beats=[p[1] for p in paired],
            )
        )
    return out


def beats_to_seconds(beat: float, tempo_map: List[TempoPoint]) -> Optional[float]:
    """Only meaningful for points >= the first recorded point (audio starts
    there) — integrates the piecewise-linear-bpm tempo map. Points carry
    recorded_at_seconds directly for source==4 (recorded/live tempo) entries,
    which is what this project uses throughout, so we interpolate between
    the two bracketing points' own recorded_at_seconds rather than
    re-deriving from bpm (avoids compounding rounding across 98 points)."""
    recorded = [p for p in tempo_map if p.recorded_at_seconds is not None]
    if not recorded:
        return None
    if beat <= recorded[0].beat:
        return recorded[0].recorded_at_seconds
    for a, b in zip(recorded, recorded[1:]):
        if a.beat <= beat <= b.beat:
            if b.beat == a.beat:
                return a.recorded_at_seconds
            frac = (beat - a.beat) / (b.beat - a.beat)
            return a.recorded_at_seconds + frac * (b.recorded_at_seconds - a.recorded_at_seconds)
    return recorded[-1].recorded_at_seconds


def load_affinity(affinity_path: Path) -> Dict[str, List[dict]]:
    """edge_vs_app_events.json shape: flat list of {layer, inst, recall,
    null, excess, offset_ms}. Grouped by layer name here."""
    rows = json.loads(affinity_path.read_text())
    by_layer: Dict[str, List[dict]] = defaultdict(list)
    for row in rows:
        by_layer[row["layer"]].append(row)
    return dict(by_layer)


def classify_layers(
    layers: List[LayerEdges], affinity_by_layer: Dict[str, List[dict]]
) -> Tuple[List[dict], List[dict]]:
    """Returns (onset_truth_rows, section_truth_rows). A layer's KEY in
    affinity_by_layer is "<NAME>#<index>" per edge_vs_app_events.json's own
    convention (verified: "CLAP#2" for the CLAP layer at index 2)."""
    onset_rows: List[dict] = []
    section_rows: List[dict] = []
    for layer in layers:
        key = f"{layer.name}#{layer.index}"
        rows = affinity_by_layer.get(key, [])
        if not rows:
            continue  # no affinity measurement for this layer — unclassified, omitted
        best = max(rows, key=lambda r: r["excess"])
        if best["excess"] >= ONSET_EXCESS_THRESHOLD:
            onset_rows.append(
                {"layer": key, "instrument": best["inst"], "excess": best["excess"], "n_edges": len(layer.edges_beats)}
            )
        elif max(r["excess"] for r in rows) < SECTION_EXCESS_CEILING:
            section_rows.append({"layer": key, "n_edges": len(layer.edges_beats), "max_excess": max(r["excess"] for r in rows)})
    return onset_rows, section_rows


def extract(project_path: Path, affinity_path: Optional[Path], out_dir: Path) -> dict:
    project = load_project_json(project_path)
    tempo_map = extract_tempo_map(project)
    layers = extract_layer_edges(project)

    out_dir.mkdir(parents=True, exist_ok=True)

    grid_out = {
        "source": project_path.name,
        "points": [asdict(p) for p in tempo_map],
    }
    (out_dir / "grid_truth.json").write_text(json.dumps(grid_out, indent=2))

    layers_out = [asdict(l) for l in layers]
    (out_dir / "layer_edges.json").write_text(json.dumps(layers_out, indent=2))

    summary = {"n_layers": len(layers), "n_tempo_points": len(tempo_map)}

    if affinity_path is not None:
        affinity_by_layer = load_affinity(affinity_path)
        onset_rows, section_rows = classify_layers(layers, affinity_by_layer)

        # Attach seconds-in-audio using the tempo map so onset/section truth
        # is directly usable against demucs-separated analysis audio.
        edges_by_key = {f"{l.name}#{l.index}": l.edges_beats for l in layers}
        for row in onset_rows + section_rows:
            beats = edges_by_key[row["layer"]]
            row["edges_beats"] = beats
            row["edges_secs_in_audio"] = [beats_to_seconds(b, tempo_map) for b in beats]

        (out_dir / "onset_truth.json").write_text(json.dumps(onset_rows, indent=2))
        (out_dir / "section_truth.json").write_text(json.dumps(section_rows, indent=2))
        summary["n_onset_truth_layers"] = len(onset_rows)
        summary["n_section_truth_layers"] = len(section_rows)
        summary["n_onset_truth_events"] = sum(r["n_edges"] for r in onset_rows)
        summary["n_section_truth_events"] = sum(r["n_edges"] for r in section_rows)
    else:
        summary["classified"] = False

    (out_dir / "manifest.json").write_text(json.dumps(summary, indent=2))
    return summary


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True, help="path to a .manifold project file")
    parser.add_argument("--affinity", type=Path, default=None, help="edge_vs_app_events.json (layer->instrument affinity); omit to skip onset/section classification")
    parser.add_argument("--out-dir", type=Path, default=Path("eval/liveshow_labels"))
    args = parser.parse_args(argv)

    summary = extract(args.project, args.affinity, args.out_dir)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
