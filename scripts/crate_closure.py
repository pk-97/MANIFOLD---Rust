#!/usr/bin/env python3
"""Crate-split census for the renderer: what the engine core pulls in, and where it reaches into a family.

Two modes, both read-only, both over `crates/manifold-nodes/src` (or `--root`):

  closure  [unit...]   transitive module closure of the seed units, with line counts.
                       A unit is `ng:<module>` (a node_graph module file or directory),
                       `prim:<name>` (one primitives file) or `root:<module>` (a
                       renderer-root module). Default seeds are the engine core named
                       in docs/RENDERER_CRATE_SPLIT_DESIGN.md D1.
  seams                every non-test line in a non-family unit that references a
                       family unit, grouped by target and by source. Family membership
                       is the table below; the design's D2 (scene vocabulary is hub)
                       and D11 (built-in primitives) are encoded there.

Edges are textual (`name::` path segments after test code is cut), so the closure is an
upper bound and the seam list a lower bound. The compiler is the oracle at execution
time; this script is for sizing and for proving "the hub names no family" negatively.
`closure` exits 0; `seams --expect-zero` exits 1 while any seam remains (the P4/P5
negative gate).
"""
import argparse
import collections
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_ROOT = os.path.join(REPO, "crates", "manifold-nodes", "src")

# RENDERER_CRATE_SPLIT_DESIGN.md D1/D2/D9: node_graph modules that are family, not engine.
# Scene vocabulary (camera, light, material, transform, ...) is deliberately absent:
# D2 keeps it in the hub.
FAMILY_NG = set(
    """liquid fluid fluid_cache fluid_role fluid_particles fluid_mesh_upload whitewater
    whitewater_handoff matter physics physics_scene physics_events physics_metrics
    gltf_import gltf_load gltf_anim_cache gltf_anim_identity scene_modifier_expand
    scene_modifier_legacy_migration scene_modifier_authoring scene_vm scene_exposure
    scene_viewport viewport_camera viewport_gizmo viewport_overlay viewport_render
    viewport_session material_inspector relight decode_cache instance_upload""".split()
)
# D11: primitives the hub may contain.
BUILTIN_PRIMS = {"wgsl_compute", "standalone_pipeline", "mix", "masked_mix", "value", "gain", "mux_texture", "mod"}
# Renderer-root modules that leave the hub (compositor, UI paint, legacy generators).
FAMILY_ROOT = set(
    """generators layer_compositor compositor tonemap headless_readback generator_renderer
    presentation display_capture preset_thumbnail ui_renderer native_text clip_draw
    clip_thumb_gpu ui_cache_manager layer_bitmap_gpu automation_lane_draw clip_content_gpu
    text_rasterizer metalfx_upscaler metalfx_temporal_upscaler fsr1 denoiser pq_encoder
    gpu_readback live_sim_clock_reference""".split()
)
DEFAULT_SEEDS = [
    "ng:execution", "ng:execution_plan", "ng:graph_loader", "ng:validation", "ng:validate",
    "ng:primitive", "ng:effect_node", "ng:freeze", "ng:persistence", "ng:graph",
    "ng:bound_graph", "ng:state_store", "ng:substeps", "ng:ports", "ng:descriptor",
    "ng:parameters", "ng:param_binding", "ng:backend", "ng:metal_backend", "ng:chain_spec",
    "ng:snapshot", "ng:resource_allocation", "root:preset_runtime", "root:preset_loader",
    "root:preset_context", "root:gpu_encoder", "root:render_target", "root:effect",
    "root:chain_dispatch",
]


def read(path):
    with open(path, encoding="utf-8", errors="ignore") as f:
        return f.read()


def is_test_file(path):
    base = os.path.basename(path)
    return "/tests/" in path or base.endswith("_tests.rs") or base == "tests.rs" or "/bin/" in path


def production_text(path):
    """File text with the trailing `#[cfg(test)]` block and `//` comment lines removed."""
    txt = read(path)
    # Keep the source offsets/line count stable while exposing production
    # declarations hidden inside testkit_visible! calls.
    from crate_move_replay import production_text as expand_testkit
    body = expand_testkit(txt)
    cut = body.find("#[cfg(test)]")
    body = body if cut < 0 else body[:cut]
    return "\n".join(l for l in body.split("\n")
                   if not l.strip().startswith("//"))


def collect_units(root):
    """unit name -> list of .rs files, named `kind:name` as the design does."""
    units = collections.defaultdict(list)

    def add(name, path):
        if os.path.isdir(path):
            for d, _, fs in os.walk(path):
                units[name].extend(os.path.join(d, f) for f in fs if f.endswith(".rs"))
        elif os.path.isfile(path) and path.endswith(".rs"):
            units[name].append(path)

    ng = os.path.join(root, "node_graph")
    prims = os.path.join(ng, "primitives")
    for e in os.listdir(root):
        n = e[:-3] if e.endswith(".rs") else e
        if n in ("node_graph", "lib", "bin", "main"):
            continue
        add("root:" + n, os.path.join(root, e))
    for e in os.listdir(ng):
        n = e[:-3] if e.endswith(".rs") else e
        if n in ("mod", "primitives"):
            continue
        add("ng:" + n, os.path.join(ng, e))
    for e in os.listdir(prims):
        n = e[:-3] if e.endswith(".rs") else e
        if n != "mod":
            add("prim:" + n, os.path.join(prims, e))
    return units


def is_family(unit):
    kind, name = unit.split(":", 1)
    if kind == "prim":
        return name not in BUILTIN_PRIMS
    if kind == "ng":
        return name in FAMILY_NG
    return name in FAMILY_ROOT


def unit_of(root, path):
    parts = os.path.relpath(path, root).split(os.sep)
    if parts[0] == "node_graph" and len(parts) > 2 and parts[1] == "primitives":
        return "prim:" + parts[2].split(".")[0]
    if parts[0] == "node_graph":
        return "ng:" + parts[1].split(".")[0]
    return "root:" + parts[0].split(".")[0]


def mode_closure(args):
    units = collect_units(args.root)
    size = {u: sum(read(f).count("\n") for f in fs) for u, fs in units.items()}
    text = {u: "\n".join(production_text(f) for f in fs if not is_test_file(f)) for u, fs in units.items()}
    pats = {u: re.compile(r"\b" + re.escape(u.split(":", 1)[1]) + r"::") for u in units}
    prim_items = {}
    for u in units:
        if u.startswith("prim:"):
            for m in re.finditer(r"pub (?:struct|enum|const|fn) (\w+)", text[u]):
                prim_items.setdefault(m.group(1), u)

    def deps(u):
        t = text[u]
        out = {v for v, p in pats.items() if v != u and p.search(t)}
        for m in re.finditer(r"primitives::\{([^}]*)\}|primitives::(\w+)", t):
            for item in re.split(r"[,\s]+", (m.group(1) or m.group(2) or "")):
                if item in prim_items:
                    out.add(prim_items[item])
                if "prim:" + item in units:
                    out.add("prim:" + item)
        out.discard(u)
        return out

    seeds = args.units or DEFAULT_SEEDS
    missing = [s for s in seeds if s not in units]
    if missing:
        print("unknown units:", ", ".join(missing), file=sys.stderr)
        return 2
    closure, why, frontier = set(seeds), {}, list(seeds)
    while frontier:
        u = frontier.pop()
        for v in deps(u):
            if v not in closure:
                closure.add(v)
                why[v] = u
                frontier.append(v)
    print(f"closure: {len(closure)} units, {sum(size[u] for u in closure)} lines (tests included)")
    by = collections.defaultdict(list)
    for u in closure:
        by[u.split(":")[0]].append(u)
    for kind in ("ng", "prim", "root"):
        us = by.get(kind, [])
        print(f"\n[{kind}] {len(us)} units, {sum(size[u] for u in us)} lines")
        for u in sorted(us, key=lambda x: -size[x])[: args.top]:
            print(f"  {size[u]:6d} {u}  <- {why.get(u, 'seed')}")
    print("\noutside the closure:")
    for kind in ("ng", "prim", "root"):
        us = [u for u in units if u not in closure and u.startswith(kind + ":")]
        print(f"  [{kind}] {len(us)} units, {sum(size[u] for u in us)} lines")
    return 0


def mode_seams(args):
    root = args.root
    prims = {f.split(".")[0] for f in os.listdir(os.path.join(root, "node_graph", "primitives"))}
    edges = collections.defaultdict(list)
    for d, _, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            if not f.endswith(".rs") or is_test_file(p):
                continue
            src = unit_of(root, p)
            if is_family(src):
                continue
            for i, line in enumerate(production_text(p).split("\n"), 1):
                where = f"{os.path.relpath(p, REPO)}:{i}"
                for m in re.finditer(r"\b([a-z_0-9]+)::", line):
                    s = m.group(1)
                    if s in FAMILY_NG:
                        tgt = "ng:" + s
                    elif s in FAMILY_ROOT:
                        tgt = "root:" + s
                    elif s in prims and s not in BUILTIN_PRIMS:
                        tgt = "prim:" + s
                    else:
                        continue
                    if tgt != src:
                        edges[(src, tgt)].append(where)
                for m in re.finditer(r"primitives::\{([^}]*)\}|primitives::([A-Z]\w+)", line):
                    for item in re.split(r"[,\s]+", (m.group(1) or m.group(2) or "")):
                        if item and item[0].isupper():
                            edges[(src, "prim-item:" + item)].append(where)
    sites = sum(len(v) for v in edges.values())
    print(f"seams: {len(edges)} edges, {sites} sites (non-test lines in non-family units)")
    by_target, by_source = collections.Counter(), collections.Counter()
    for (s, t), v in edges.items():
        by_target[t] += len(v)
        by_source[s] += len(v)
    print("\nby target:")
    for t, c in by_target.most_common(args.top):
        print(f"  {c:4d} {t}")
    print("\nby source:")
    for s, c in by_source.most_common(args.top):
        print(f"  {c:4d} {s}")
    if args.sites:
        print("\nsites:")
        for (s, t), v in sorted(edges.items(), key=lambda kv: (-len(kv[1]), kv[0])):
            print(f"  {s} -> {t}")
            for site in v:
                print(f"      {site}")
    return 1 if (args.expect_zero and sites) else 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--root", default=DEFAULT_ROOT, help="renderer src root")
    ap.add_argument("--top", type=int, default=40)
    sub = ap.add_subparsers(dest="mode", required=True)
    c = sub.add_parser("closure")
    c.add_argument("units", nargs="*")
    s = sub.add_parser("seams")
    s.add_argument("--sites", action="store_true", help="print every file:line")
    s.add_argument("--expect-zero", action="store_true", help="exit 1 if any seam remains")
    args = ap.parse_args(argv)
    return mode_closure(args) if args.mode == "closure" else mode_seams(args)


if __name__ == "__main__":
    sys.exit(main())
