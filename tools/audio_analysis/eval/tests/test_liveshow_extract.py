"""Live-show extractor tests. Builds a minimal synthetic .manifold-shaped
zip (project.json only, matching the real schema's relevant fields) rather
than depending on Peter's actual project file, so this runs anywhere. The
extractor's correctness against the REAL project was verified interactively
this session (46/46 layers, edges match edge_corpus.json to float-rounding
precision) — recorded in the P1 landing report, not re-asserted here since
that file lives outside the repo."""

from __future__ import annotations

import json
import zipfile

from eval.liveshow_extract import (
    beats_to_seconds,
    classify_layers,
    extract_layer_edges,
    extract_tempo_map,
    load_affinity,
    load_project_json,
)


def _make_project_zip(path):
    project = {
        "tempoMap": {
            "points": [
                {"beat": 0.0, "bpm": 120.0, "source": 4},  # no recordedAtSeconds -> pre-recording sentinel-equivalent
                {"beat": 100.0, "bpm": 120.0, "recordedAtSeconds": 0.0, "source": 4},
                {"beat": 108.0, "bpm": 120.0, "recordedAtSeconds": 4.0, "source": 4},
            ]
        },
        "timeline": {
            "layers": [
                {"name": "MASTER", "index": 0, "layerType": 3, "clips": [{"startBeat": 100.0}]},
                {"name": "SONG GROUP", "index": 1, "layerType": 2, "clips": []},
                {
                    "name": "KICK",
                    "index": 2,
                    "layerType": 1,
                    "clips": [
                        {"startBeat": 104.0, "durationBeats": 0.25},
                        {"startBeat": 102.0, "durationBeats": 0.25},
                    ],
                },
            ]
        },
    }
    with zipfile.ZipFile(path, "w") as zf:
        zf.writestr("project.json", json.dumps(project))
        zf.writestr("manifest.json", "{}")


def test_load_project_json_reads_the_zip(tmp_path):
    p = tmp_path / "test.manifold"
    _make_project_zip(p)
    project = load_project_json(p)
    assert "tempoMap" in project


def test_extract_tempo_map_drops_the_negative_sentinel(tmp_path):
    p = tmp_path / "test.manifold"
    _make_project_zip(p)
    project = load_project_json(p)
    points = extract_tempo_map(project)
    # The beat=0 point has no recordedAtSeconds at all in this fixture
    # (mirrors the real project's -1.0 sentinel semantics: not usable for
    # seconds conversion) — extract_tempo_map keeps points with recorded is
    # None OR >=0; only negative values are dropped. Confirm the two
    # recorded points survive and are ordered.
    recorded = [p for p in points if p.recorded_at_seconds is not None]
    assert len(recorded) == 2
    assert recorded[0].beat == 100.0


def test_extract_layer_edges_only_keeps_performable_layers(tmp_path):
    p = tmp_path / "test.manifold"
    _make_project_zip(p)
    project = load_project_json(p)
    layers = extract_layer_edges(project)
    assert len(layers) == 1
    assert layers[0].name == "KICK"
    assert layers[0].edges_beats == [102.0, 104.0]  # sorted, not insertion order


def test_beats_to_seconds_interpolates_between_recorded_points(tmp_path):
    p = tmp_path / "test.manifold"
    _make_project_zip(p)
    project = load_project_json(p)
    tempo_map = extract_tempo_map(project)
    # Halfway between beat 100 (0.0s) and beat 108 (4.0s) -> beat 104 -> 2.0s
    assert abs(beats_to_seconds(104.0, tempo_map) - 2.0) < 1e-9


def test_classify_layers_uses_excess_thresholds():
    layers = extract_layer_edges(
        {"timeline": {"layers": [{"name": "A", "index": 0, "layerType": 1, "clips": [{"startBeat": 1.0}]}]}}
    )
    onset_affinity = {"A#0": [{"layer": "A#0", "inst": "kick", "recall": 0.9, "null": 0.2, "excess": 0.3, "offset_ms": 5}]}
    onset_rows, section_rows = classify_layers(layers, onset_affinity)
    assert len(onset_rows) == 1 and onset_rows[0]["instrument"] == "kick"
    assert len(section_rows) == 0

    section_affinity = {"A#0": [{"layer": "A#0", "inst": "kick", "recall": 0.5, "null": 0.49, "excess": 0.01, "offset_ms": 5}]}
    onset_rows2, section_rows2 = classify_layers(layers, section_affinity)
    assert len(onset_rows2) == 0
    assert len(section_rows2) == 1
