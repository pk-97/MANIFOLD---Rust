#!/usr/bin/env python3
"""Capture importer def-JSON dumps for the P3-D INV-R8 equivalence gate.

The `render-import --dump-def <glb> <out.json>` mode serializes the
`(EffectGraphDef, ImportReport)` a fixture assembles into through the SAME
production entry point (`assemble_import_graph`). This script drives that mode
over a fixture list and writes one dump per fixture plus a `manifest.json`
mapping each fixture's relative path to its dump's sha256.

The equivalence proof (RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md INV-R8): dump
every fixture at the pre-change commit, apply a table-ization slice, re-dump,
byte-diff the two capture dirs. A pure catalog->table refactor MUST leave every
dump byte-identical (importer output is deterministic — BTreeMap params, counter
node ids, lookup-only HashMaps).

LOUD-FAIL contract (D4 / the named forbidden inheritance): a MISSING fixture is
a hard nonzero exit — never the if-present self-skip the AMG GT3 test uses. A
fixture whose import ERRORS still produces a dump (an `{"import_error": ...}`
sentinel written by the bin), so error behavior is frozen by the diff too; the
script only fails on a missing file or a bin crash (nonzero subprocess exit).

Usage:
    scripts/gltf_def_capture.py --bin <render-import-binary> --out <dir> \
        [--fixtures-root <dir>] [fixture ...]

    --bin           path to a prebuilt `render-import` binary (avoids a
                    145x cargo rebuild); if omitted, falls back to
                    `cargo run -p manifold-renderer --bin render-import`.
    --out           capture output directory (created; NOT the repo).
    --fixtures-root root to enumerate *.glb / *.gltf under when no explicit
                    fixtures are given (default: <repo>/tests/fixtures/gltf).
    fixture...      explicit fixture paths; overrides enumeration.
"""

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path


def dump_key(fixture: Path, root: Path) -> str:
    """Unique per-fixture dump stem: the fixture's path relative to `root`
    with separators flattened, so `DamagedHelmet.glb` and
    `khronos/DamagedHelmet.glb` do not collide."""
    try:
        rel = fixture.relative_to(root)
    except ValueError:
        rel = Path(fixture.name)
    return str(rel.with_suffix("")).replace("/", "__")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", type=Path, default=None)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--fixtures-root", type=Path, default=None)
    ap.add_argument("fixtures", nargs="*", type=Path)
    args = ap.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    fixtures_root = (args.fixtures_root or (repo_root / "tests/fixtures/gltf")).resolve()

    if args.fixtures:
        fixtures = [Path(f).resolve() for f in args.fixtures]
    else:
        if not fixtures_root.is_dir():
            print(f"FATAL: fixtures root does not exist: {fixtures_root}", file=sys.stderr)
            return 1
        fixtures = sorted(
            {p.resolve() for ext in ("*.glb", "*.gltf") for p in fixtures_root.rglob(ext)}
        )

    if not fixtures:
        print(f"FATAL: no fixtures found under {fixtures_root}", file=sys.stderr)
        return 1

    out_dir = args.out.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)

    manifest: dict[str, str] = {}
    for fixture in fixtures:
        if not fixture.exists():
            print(f"FATAL: missing fixture (no if-present skip): {fixture}", file=sys.stderr)
            return 1
        key = dump_key(fixture, fixtures_root)
        dump_path = out_dir / f"{key}.json"
        if args.bin:
            cmd = [str(args.bin), "--dump-def", str(fixture), str(dump_path)]
        else:
            cmd = [
                "cargo", "run", "-q", "-p", "manifold-renderer",
                "--bin", "render-import", "--", "--dump-def", str(fixture), str(dump_path),
            ]
        result = subprocess.run(cmd)
        if result.returncode != 0:
            print(
                f"FATAL: render-import crashed (exit {result.returncode}) on {fixture}",
                file=sys.stderr,
            )
            return 1
        if not dump_path.exists():
            print(f"FATAL: no dump written for {fixture}", file=sys.stderr)
            return 1
        manifest[key] = hashlib.sha256(dump_path.read_bytes()).hexdigest()

    manifest_path = out_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"captured {len(manifest)} dumps -> {out_dir}")
    print(f"manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
