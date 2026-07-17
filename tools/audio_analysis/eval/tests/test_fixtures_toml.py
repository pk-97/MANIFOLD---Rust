"""fixtures.toml sanity: parses, D9 split values are valid, every fixture
has a license note (D8's audit discipline)."""

from __future__ import annotations

from pathlib import Path

try:
    import tomllib
except ImportError:
    import tomli as tomllib  # type: ignore[no-redef]

FIXTURES_PATH = Path(__file__).resolve().parents[1] / "fixtures.toml"
VALID_SPLITS = {"dev", "heldout", "full", "dev_and_heldout"}


def _load():
    with open(FIXTURES_PATH, "rb") as f:
        return tomllib.load(f)


def test_fixtures_toml_parses():
    data = _load()
    assert "fixture" in data
    assert len(data["fixture"]) > 0


def test_every_fixture_has_required_fields():
    data = _load()
    for fx in data["fixture"]:
        assert "id" in fx, fx
        assert "split" in fx, fx["id"]
        assert fx["split"] in VALID_SPLITS, f"{fx['id']}: invalid split {fx['split']!r}"
        assert "license" in fx, fx["id"]
        assert "roles" in fx, fx["id"]


def test_fixture_ids_are_unique():
    data = _load()
    ids = [fx["id"] for fx in data["fixture"]]
    assert len(ids) == len(set(ids)), "duplicate fixture ids in fixtures.toml"


def test_kick_onset_fixtures_have_label_paths_and_tolerance():
    data = _load()
    for fx in data["fixture"]:
        if fx.get("truth_kind") == "kick_onset_csv":
            assert "labels_path" in fx, fx["id"]
            assert "bpm" in fx, fx["id"]
            assert "instrument_filter" in fx, fx["id"]


def test_liveshow_fixtures_have_beat_ranges():
    data = _load()
    for fx in data["fixture"]:
        if fx.get("dataset") == "liveshow":
            assert "beat_range" in fx, fx["id"]
            lo, hi = fx["beat_range"]
            assert lo < hi, fx["id"]
