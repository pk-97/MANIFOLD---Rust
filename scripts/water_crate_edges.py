#!/usr/bin/env python3
"""Water crate edge census (docs/WATER_CRATES_DESIGN.md section 1.2).

Places every file of crates/manifold-nodes-water/src in the crate section 3
of the design gives it, resolves each `crate::` and `super::` path, and prints
the area-to-area edge table and every CUT site: a production reach that is not
downward (leaf -> liquid -> rigid) or from the registration crate. Test files
and `#[cfg(test)]` bodies are counted apart; BUG-hkbdp.6.11 (water test cut)
places them. `--check` exits 1 on any CUT row or any file no area claims.

Obsolete when the crates exist (stage 5 of the design): Cargo holds the lines.
"""
import argparse
import collections
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SRC = ROOT / "crates" / "manifold-nodes-water" / "src"

P = r"^primitives/"
# First match wins; order puts the narrow rules before the broad ones.
AREAS = [
    ("top", r"^(lib|graph_install|wire_values|physics_scene|live_sim_clock_reference|migration|presets)(\.rs|/)"
            r"|^runtime/|^primitives/(mod|testkit)\.rs$|^testkit/mod\.rs$"
            r"|^testkit/(conformance|preset_extents|face_grid_scenes|whitewater_scene|whitewater_fingerprints)(\.rs|/)"
            r"|^liquid/scene_contract\.rs$|^primitives/face_grid_(tests|extent_tests)\.rs$"
            r"|^primitives/liquid_bricks_consumer_tests\.rs$|^primitives/sort_particles_into_cells/gpu_tests\.rs$"),
    ("rigid", r"^(node|coupled_frame|vector_field|physics|physics_mesh|physics_events|physics_metrics)(\.rs|/)"
              r"|^primitives/(physics_world|rigid_body|vector_fields)(\.rs|/)|^testkit/physics_fixtures\.rs$"),
    ("liquid", r"^(liquid|fluid_role|fluid_particles|whitewater)(\.rs|/)"
               r"|^testkit/(fluid_role_source|liquid_extents|particle_volume|liquid_surface)\.rs$|^primitives/testkit/liquid\.rs$"
               + "|" + P + r"(fluid_role_source|sort_particles_into_cells|prefix_scan|face_sample_component|liquid_cells"
               r"|liquid_stats|particle_identity|particle_publication|liquid_bricks|smooth_lattice|redistance_lattice"
               r"|offset_lattice|upwind_distance|whitewater_distance|running_total|dot_products)(\.rs|/)"),
    ("matter", r"^matter(\.rs|/)|^primitives/(matter_\w+|grid_to_matter)(\.rs|/)"),
    ("whitewater", r"^whitewater_handoff\.rs$|^testkit/water_codegen\.rs$|^primitives/testkit/whitewater\.rs$"
                   + "|" + P + r"(whitewater_\w+|dust_potential|emission_count|energy_potential|inside_turbulence_potential"
                   r"|turbulence_emission_count|turbulence_field|wavecrest_potential|crossing_distance|nearest_crossing"
                   r"|surface_crossings|keep_whitewater|advect_whitewater|age_whitewater|retype_whitewater|spawn_whitewater"
                   r"|preserve_foam|jitter_particles|sample_faces_at_particles|extend_lattice|lattice_curvature"
                   r"|pad_distance_lattice)(\.rs|/)"),
    ("gpuflip", r"^primitives/testkit/gpu_flip\.rs$"
                + "|" + P + r"(gpu_flip_\w+|liquid_state|liquid_fill|liquid_solid_distance|clamp_liquid_to_solids"
                r"|push_out_of_solid|euler_step_particles(_3d)?|apply_radial_burst(_3d)?_to_particles|liquid_surface_tests)(\.rs|/)"),
    ("surface", r"^primitives/testkit/surface\.rs$"
                + "|" + P + r"(lattice_bricks|lattice_closing_tests|liquid_frame|particle_volume|volume_surface_mesh"
                r"|count_surface_edges|count_surface_triangles|relax_surface_mesh|smooth_surface_mesh|surface_mesh_normals"
                r"|surface_mesh_parity|shape_particle_blobs|blob_bounds)(\.rs|/)"),
]
DOWN = {"liquid": {"rigid"}, "rigid": set(), "top": None, "UNPLACED": set()}
for leaf in ("gpuflip", "matter", "whitewater", "surface"):
    DOWN[leaf] = {"liquid", "rigid"}
TEST_FILE = re.compile(r"_tests?\.rs$|/tests?(/|\.rs)|testkit|_cpu\.rs$|reference\.rs$|live_sim_clock")
TEST_MOD = re.compile(r"#\[cfg\((?:all\()?test[^\n]*\n\s*(?:pub(?:\(crate\))? )?mod \w+ \{")


def area(rel):
    for name, rx in AREAS:
        if re.search(rx, rel):
            return name
    return None


def census(src):
    src = Path(src)
    files = {}
    for d, _, names in os.walk(src):
        for n in names:
            if n.endswith(".rs"):
                rel = os.path.relpath(os.path.join(d, n), src).replace(os.sep, "/")
                files[rel] = area(rel) or "UNPLACED"
    mods = {rel[:-3].replace("/mod", "").replace("/", "::"): rel for rel in files}
    prod = collections.defaultdict(set)
    test = collections.Counter()
    for rel, a in files.items():
        text = (src / rel).read_text()
        cut = TEST_MOD.search(text)
        body = text[:cut.start()] if cut else text
        refs = re.findall(r"crate::((?:\w+::)*\w+)", body)
        parent = "::".join(rel[:-3].replace("/mod", "").split("/")[:-1])
        refs += [(parent + "::" + m) if parent else m for m in re.findall(r"super::((?:\w+::)*\w+)", body)]
        for ref in refs:
            parts = ref.split("::")
            for k in range(len(parts), 0, -1):
                target = "::".join(parts[:k])
                if target in mods:
                    b = files[mods[target]]
                    if target != "primitives" and b != a:
                        if TEST_FILE.search(rel):
                            test[(a, b)] += 1
                        else:
                            prod[(a, b)].add((rel, ref))
                    break
    return files, prod, test


def is_cut(a, b):
    allowed = DOWN.get(a)
    return allowed is not None and b not in allowed


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("src", nargs="?", default=str(DEFAULT_SRC))
    ap.add_argument("--check", action="store_true", help="exit 1 on any CUT row or unplaced file")
    ap.add_argument("-v", action="store_true", help="also print every file's area")
    args = ap.parse_args(argv)
    files, prod, test = census(args.src)
    unplaced = sorted(f for f, a in files.items() if a == "UNPLACED")
    print("files per area:", dict(sorted(collections.Counter(files.values()).items())))
    print("\nproduction edges:")
    for (a, b), sites in sorted(prod.items(), key=lambda x: (-len(x[1]), x[0])):
        print(f"  {a:10s} -> {b:10s} {len(sites):4d}  {'CUT' if is_cut(a, b) else 'ok'}")
    print("\ntest-only edges (BUG-hkbdp.6.11):", dict(sorted(test.items())))
    cuts = {k: v for k, v in prod.items() if is_cut(*k)}
    print(f"\nCUT rows: {len(cuts)}")
    for (a, b), sites in sorted(cuts.items()):
        print(f"--- {a} -> {b}")
        for rel, ref in sorted(sites):
            print(f"    {rel}  ->  {ref}")
    for f in unplaced:
        print(f"UNPLACED {f}")
    if args.v:
        for f in sorted(files):
            print(f"  {files[f]:10s} {f}")
    return 1 if args.check and (cuts or unplaced) else 0


if __name__ == "__main__":
    sys.exit(main())
