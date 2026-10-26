#!/usr/bin/env python3
"""Validate the hand-composed water shaders with naga in a few seconds.

Each stack is stitched from source files the way the Rust `shader()` /
`step_source()` functions stitch it, then handed to `naga` (install with
`cargo install naga-cli`). No cargo build, no GPU. Run with no args for every
stack, or name stacks: `scripts/wgsl_check.py step sheeting`.

Keep a stack's file list in step with its Rust composition when that changes.
"""

import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
C = ROOT / "crates"
LIQ = C / "manifold-water-liquid/src/primitives/shaders"
FLIP = C / "manifold-water-gpu-flip/src/primitives/shaders"
WW = C / "manifold-water-whitewater/src/primitives/shaders"

TILE_STORAGE = [LIQ / "tile_grid.wgsl", LIQ / "tile_storage.wgsl"]
STEP_PARTS = [
    "common", "transfer", "extend", "forces", "solids", "pocket_bindings",
    "tile_bindings", "pockets", "divergence", "particle_distance",
    "projection", "density", "move", "emit", "tiles", "marker_removal",
]


def stats_layout() -> str:
    """The `with_stats_layout` const lines, values read from liquid_stats.rs."""
    src = (C / "manifold-water-liquid/src/primitives/liquid_stats.rs").read_text()
    values: dict[str, int] = {}
    for name, expr in re.findall(r"pub const (\w+): u32 = ([^;]+);", src):
        # Definition order: each constant only names ones defined above it.
        values[name] = int(eval(expr, {}, dict(values)))  # noqa: S307 - our own integer consts

    names = ["SOLVER_STATS_START", "SOLVER_WORDS", "INFLOW_BIRTHS_TAIL",
             "INFLOW_REFUSED_TAIL", "SHEET_REFUSED_TAIL"]
    return "".join(f"const {n}: u32 = {values[n]}u;\n" for n in names) + "\n"


STACKS = {
    "step": (True, [LIQ / "liquid_pose.wgsl", LIQ / "liquid_collider.wgsl",
                    C / "manifold-nodes-physics/src/shaders/force_lattice.wgsl",
                    *TILE_STORAGE,
                    *[FLIP / "gpu_flip_step" / f"{p}.wgsl" for p in STEP_PARTS]]),
    "pressure": (False, [*TILE_STORAGE, FLIP / "gpu_flip_pressure.wgsl"]),
    "coarse_inverse": (False, [*TILE_STORAGE, FLIP / "coarse_inverse.wgsl"]),
    "bodies": (False, [*TILE_STORAGE, FLIP / "gpu_flip_bodies.wgsl"]),
    "lentine": (False, [LIQ / "tile_grid.wgsl", FLIP / "gpu_flip_lentine.wgsl"]),
    "sheeting": (True, [*TILE_STORAGE, FLIP / "gpu_flip_sheeting.wgsl"]),
    "sheet_distance": (False, [*TILE_STORAGE, FLIP / "gpu_flip_sheet_distance.wgsl"]),
    "ww_gather": (False, [*TILE_STORAGE, WW / "gather_distance.wgsl"]),
    "ww_fused": (False, [LIQ / "tile_grid.wgsl", LIQ / "whitewater_common.wgsl",
                         LIQ / "liquid_faces.wgsl",
                         C / "manifold-nodes-physics/src/shaders/force_lattice.wgsl",
                         WW / "whitewater_fused.wgsl"]),
}


def check(name: str, stats: bool, files: list[Path], tmp: Path) -> bool:
    source = "\n".join(f.read_text() for f in files)
    if stats:
        source = stats_layout() + source
    out = tmp / f"{name}.wgsl"
    out.write_text(source)
    run = subprocess.run(["naga", str(out)], capture_output=True, text=True)
    if run.returncode == 0:
        print(f"ok    {name}")
        return True
    print(f"FAIL  {name}  (composed source: {out})")
    print((run.stderr or run.stdout).rstrip())
    return False


def main() -> int:
    wanted = sys.argv[1:] or list(STACKS)
    unknown = [w for w in wanted if w not in STACKS]
    if unknown:
        print(f"unknown stack(s): {unknown}; known: {list(STACKS)}")
        return 2
    tmp = Path(tempfile.mkdtemp(prefix="wgsl_check_"))
    results = [check(n, *STACKS[n], tmp) for n in wanted]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
