"""Invariant gate (docs/AUDIO_EVENT_CLASSIFIER_DESIGN.md §4): every
train/sources.toml entry names an allowed license, and no file in train/
(nor sources.toml itself) references a banned dataset by name -- per D6,
non-consensual-license model outputs must never become training labels,
and certain research-only audio corpora must never enter training. Also
enforces the P1 heldout-discipline FORBIDDEN: zero references to the
ship-candidate-reserved liveshow songs (or the split name itself) anywhere
in train/."""

from __future__ import annotations

import re
from pathlib import Path

try:
    import tomllib
except ImportError:  # pragma: no cover - runtime is 3.12, tomllib always present
    import tomli as tomllib  # type: ignore[no-redef]

TRAIN_DIR = Path(__file__).resolve().parents[2] / "train"
SOURCES_TOML = TRAIN_DIR / "sources.toml"
ALLOWED_LICENSES = {"CC-BY", "CC0", "ours"}

# D6: "Forbidden by name: ADTOF/madmom outputs as labels; Slakh/MUSDB audio
# in any training set." This pattern is deliberately not spelled out in any
# train/ file's own comments (see sources.toml's header) so this test is
# the ONE place the names live in train/'s vicinity.
BANNED_PATTERN = re.compile(r"slakh|musdb|adtof|madmom", re.IGNORECASE)
HELDOUT_PATTERN = re.compile(r"stagnate|basalt|heldout", re.IGNORECASE)
SCANNED_SUFFIXES = (".py", ".toml", ".md")


def _load_sources():
    with open(SOURCES_TOML, "rb") as f:
        return tomllib.load(f)["source"]


def _train_text_files():
    return [p for p in sorted(TRAIN_DIR.rglob("*")) if p.is_file() and p.suffix in SCANNED_SUFFIXES]


def test_sources_toml_exists():
    assert SOURCES_TOML.exists()


def test_every_source_has_an_allowed_license():
    sources = _load_sources()
    assert sources, "sources.toml has no [[source]] entries"
    for entry in sources:
        assert entry["license"] in ALLOWED_LICENSES, f"{entry['id']}: license {entry['license']!r} not allowlisted"


def test_every_source_names_id_loader_and_class_coverage():
    sources = _load_sources()
    for entry in sources:
        assert entry.get("id"), f"entry missing id: {entry}"
        assert entry.get("loader"), f"{entry.get('id')}: missing loader"
        assert entry.get("classes"), f"{entry.get('id')}: missing class coverage"
        assert entry.get("path"), f"{entry.get('id')}: missing path"


def test_no_banned_dataset_references_anywhere_in_train_dir():
    files = _train_text_files()
    assert files, "no scannable files found in train/"
    for path in files:
        text = path.read_text()
        m = BANNED_PATTERN.search(text)
        assert m is None, f"{path.relative_to(TRAIN_DIR)}: banned dataset reference {m.group(0)!r}"


def test_no_heldout_references_anywhere_in_train_dir():
    """FORBIDDEN (P1 brief): zero references to the ship-candidate-reserved
    liveshow songs (or the split name itself) anywhere in train/."""
    files = _train_text_files()
    for path in files:
        text = path.read_text()
        m = HELDOUT_PATTERN.search(text)
        assert m is None, f"{path.relative_to(TRAIN_DIR)}: forbidden heldout-adjacent reference {m.group(0)!r}"
