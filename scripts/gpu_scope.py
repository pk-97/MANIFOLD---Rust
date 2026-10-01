#!/usr/bin/env python3
"""GPU-proofs scope selection: touched paths -> the focused set of GPU tests.

Single source for scripts/gpu_proofs_gate.py (default mode, dev and landing),
scripts/landing_gate.py and scripts/codex_checks.py. Rules:

- Every touched GPU path maps to test filters, plus the fixed SMOKE set.
- A GPU path with no mapping is a hard failure naming the path; the author adds
  a rule here. There is no run-everything fallback. Everything runs only with
  `gpu_proofs_gate.py --all` (nightly trunk_health.py).
- Scoped runs skip every test whose measured time (scripts/gpu_test_times.json)
  is over SLOW_THRESHOLD_S; there is no hand-kept list.
- glb_conformance (the ~16-minute glTF sample sweep) runs only when glTF import
  paths are touched, and is exempt from the time budget.
- manifold-gpu core, shared WGSL and the proof harness map to BROAD, a bounded
  set (named below), never to everything.

Filters are libtest substring filters applied to the renderer lib binary
(primitive `gpu_tests`, freeze, graph runtime) and the `gpu_proofs` binary.

Obsolete when: the GPU test suite is fast enough to run whole at every landing.
"""

import json
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

RENDERER_SRC = "crates/manifold-renderer/src/"
PROOFS_DIR = "crates/manifold-renderer/tests/gpu_proofs/"

# Landing ceiling for the scoped (non-glb) GPU step, seconds of test run time.
LANDING_BUDGET_S = 300

# Fixed end-to-end smoke: always runs when any GPU path is touched. Four proofs
# that cover the effect chain + alpha contract, command-buffer replay, the
# camera/scene render, and the G-buffer. Must stay under ~2 minutes in total;
# the 25-slowest timing report is how that is re-checked.
SMOKE_FILTERS = [
    "alpha_contract::effects_preserve_transparency",
    "encode_replay::encode_replay_parity",
    "camera_conformance::render_scene_matches_project_to_pixel_oracle",
    "gbuffer_depth::gbuffer_depth_conformance",
]

# Graph runtime + freeze compiler.
RUNTIME_FILTERS = [
    "node_graph::freeze::",
    "node_graph::execution",
    "node_graph::resource_allocation",
    "node_graph::metal_backend",
    "node_graph::bindings",
    "node_graph::graph_loader",
    "preset_runtime::",
]

# manifold-gpu core, shared WGSL, proof harness: runtime set + lighting proofs.
BROAD_FILTERS = RUNTIME_FILTERS + ["render_scene_lights"]

# Tests measured slower than this are skipped by scoped runs (nightly --all runs
# them). The measurements live in scripts/gpu_test_times.json, written by
# `gpu_proofs_gate.py --all --record-times PATH` (nightly trunk_health does this
# into /tmp; a human commits the refresh). A test missing from the file runs.
SLOW_THRESHOLD_S = 60
TIMES_PATH = Path(__file__).resolve().parent / "gpu_test_times.json"


def load_times(path=None):
    """{test name: seconds} from the measured-times file; {} if absent."""
    path = Path(path or TIMES_PATH)
    if not path.exists():
        return {}
    return json.loads(path.read_text()).get("tests", {})


def slow_tests(times=None):
    """[(name, seconds)] measured over SLOW_THRESHOLD_S, slowest first."""
    times = load_times() if times is None else times
    return sorted(((n, s) for n, s in times.items() if s > SLOW_THRESHOLD_S),
                  key=lambda t: -t[1])


# A shader included by more primitives than this is "shared WGSL" -> BROAD.
SHARED_WGSL_USERS = 12

# Explicit rows: (path substrings, (filters, skips)). `rt_` skips particletext:
# the freeze proof `particletext_*` hangs the GPU on main (BUG-i6eo).
EXPLICIT_ROWS = [
    (("crates/manifold-gpu/src/metal/raytrace.rs",
      RENDERER_SRC + "node_graph/primitives/render_scene.rs",
      RENDERER_SRC + "node_graph/primitives/shaders/render_scene.wgsl",
      PROOFS_DIR + "rt_"),
     (["rt_"], ["particletext"])),
    ((RENDERER_SRC + "node_graph/freeze/",), (["freeze::"], [])),
    # Live Matter (GPU_MPM_SOLVER_DESIGN.md) and the substep regions it runs in.
    ((RENDERER_SRC + "node_graph/matter.rs",
      RENDERER_SRC + "node_graph/matter/",
      RENDERER_SRC + "node_graph/substeps.rs",
      RENDERER_SRC + "node_graph/execution/substep_region.rs",
      RENDERER_SRC + "node_graph/primitives/matter_",
      RENDERER_SRC + "node_graph/primitives/grid_to_matter",
      RENDERER_SRC + "node_graph/primitives/zero_array",
      RENDERER_SRC + "node_graph/primitives/shaders/matter_",
      RENDERER_SRC + "node_graph/primitives/shaders/grid_to_matter",
      RENDERER_SRC + "node_graph/primitives/shaders/zero_array",
      PROOFS_DIR + "matter_",
      PROOFS_DIR + "substeps"),
     (["matter_", "substeps_"], [])),
    # Graph runtime.
    ((RENDERER_SRC + "node_graph/execution",
      RENDERER_SRC + "node_graph/resource_allocation",
      RENDERER_SRC + "node_graph/metal_backend",
      RENDERER_SRC + "node_graph/backend.rs",
      RENDERER_SRC + "node_graph/bound_graph.rs",
      RENDERER_SRC + "node_graph/graph.rs",
      RENDERER_SRC + "node_graph/graph_loader.rs",
      RENDERER_SRC + "node_graph/bindings",
      RENDERER_SRC + "node_graph/effect_node.rs",
      RENDERER_SRC + "node_graph/primitive.rs",
      RENDERER_SRC + "gpu_encoder.rs"),
     (RUNTIME_FILTERS, [])),
]

# Paths whose change affects every proof: BROAD.
BROAD_PATHS = (
    RENDERER_SRC + "node_graph/primitives/mod.rs",
    RENDERER_SRC + "node_graph/mod.rs",
    RENDERER_SRC + "lib.rs",
    PROOFS_DIR + "harness.rs",
    PROOFS_DIR + "main.rs",
)

GLTF_PATHS = (
    "crates/manifold-renderer/tests/glb_conformance.rs",
    "tests/fixtures/gltf/",
    RENDERER_SRC + "node_graph/gltf_",
    RENDERER_SRC + "node_graph/primitives/gltf_",
)

DOC_SUFFIXES = (".md", ".txt")

# Renderer files outside node_graph/ whose lib tests include GPU proofs.
# preset_runtime/ drives every graph; layer_skin.rs's end-to-end proofs live
# in preset_runtime's tests, so its row names both modules.
PRESET_RUNTIME_DIR = RENDERER_SRC + "preset_runtime/"
LIB_PROOF_ROWS = {
    RENDERER_SRC + "layer_skin.rs": ["layer_skin::", "preset_runtime::layer_skin_tests::"],
}

PATH_ATTR_MOD = re.compile(r'#\[path\s*=\s*"tests/([\w.]+)"\]\s*mod\s+(\w+)\s*;')


def is_gpu_path(path):
    """Paths that trigger the GPU-proofs leg (mirrors the context-nudge triggers)."""
    if path.endswith(".wgsl"):
        return True
    if path.startswith("crates/manifold-gpu/") or path.startswith(RENDERER_SRC + "node_graph/"):
        return True
    if "shaders/" in path or "gpu_encoder" in path:
        return True
    if path.startswith(PRESET_RUNTIME_DIR) or path in LIB_PROOF_ROWS:
        return True
    return "tests/gpu_proofs/" in path or is_gltf_path(path)


def is_gltf_path(path):
    return any(path.startswith(p) for p in GLTF_PATHS)


@dataclass
class Plan:
    paths: list = field(default_factory=list)       # GPU paths considered
    filters: set = field(default_factory=set)
    skips: set = field(default_factory=set)
    glb: bool = False
    broad: list = field(default_factory=list)        # (path, reason) that mapped to BROAD
    unmapped: list = field(default_factory=list)     # (path, why)
    notes: list = field(default_factory=list)

    @property
    def active(self):
        return bool(self.paths)

    def final_filters(self):
        return sorted(set(SMOKE_FILTERS) | self.filters)

    def final_skips(self):
        # A skip that would hide a filter we deliberately selected is dropped.
        skips = set(self.skips) | {n for n, _ in slow_tests()}
        return sorted(s for s in skips if not any(s in f for f in self.filters))

    def runs(self):
        """[{targets, lib, filters, skips, budgeted}] cargo invocations to make."""
        if not self.active:
            return []
        runs = [{"targets": ["gpu_proofs"], "lib": True, "filters": self.final_filters(),
                 "skips": self.final_skips(), "budgeted": True}]
        if self.glb:
            runs.append({"targets": ["glb_conformance"], "lib": False, "filters": [],
                         "skips": [], "budgeted": False})
        return runs

    def describe(self):
        lines = [f"{len(self.paths)} GPU path(s) touched; smoke + mapped filters"]
        lines.append(f"  filters: {', '.join(self.final_filters())}")
        if self.final_skips():
            lines.append(f"  skips: {', '.join(self.final_skips())}")
        for name, secs in slow_tests():
            lines.append(f"  {name}: skipped, run nightly only (measured {secs:.0f}s)")
        if self.broad:
            lines.append("  broad set (runtime + lighting) because: " +
                         "; ".join(f"{p} ({why})" for p, why in self.broad))
        if self.glb:
            lines.append("  glb_conformance: glTF import paths touched (exempt from time budget)")
        return "\n".join(lines)


def module_filters(path):
    """Lib/proof test-path filters for a renderer source file, or None for root."""
    parts = path[len(RENDERER_SRC):].split("/")
    name = parts[-1]
    if not name.endswith(".rs"):
        return []
    stem, dirs = name[:-3], parts[:-1]
    if stem in ("mod", "lib", "main"):
        mods = [dirs]
    else:
        mods = [dirs + [stem]]
        for suffix in ("_gpu_tests", "_tests"):
            if stem.endswith(suffix) and len(stem) > len(suffix):
                mods.append(dirs + [stem[: -len(suffix)]])
        if stem in ("tests", "gpu_tests"):
            mods.append(dirs)
    return ["::".join(m) + "::" for m in mods if m]


def path_attr_filters(path, repo):
    """Filters for a `<dir>/tests/<file>.rs` pulled in by `#[path] mod x;` in `<dir>/mod.rs`.

    The test module is named by that declaration, not by the file path, so the
    path-derived filter would select nothing. Unresolvable preset_runtime test
    files fall back to the whole preset_runtime module rather than to nothing.
    """
    parts = path[len(RENDERER_SRC):].split("/")
    if len(parts) < 3 or parts[-2] != "tests":
        return None
    dirs = parts[:-2]
    try:
        text = (Path(repo) / RENDERER_SRC / "/".join(dirs) / "mod.rs").read_text()
    except OSError:
        text = ""
    for file_name, module in PATH_ATTR_MOD.findall(text):
        if file_name == parts[-1]:
            return ["::".join(dirs + [module]) + "::"]
    if path.startswith(PRESET_RUNTIME_DIR):
        return ["preset_runtime::"]
    return None


def default_shader_users(repo, wgsl_path, depth=3):
    """Rust files that (transitively through other .wgsl) include `wgsl_path`."""
    found, frontier, seen = set(), [wgsl_path], {wgsl_path}
    for _ in range(depth):
        nxt = set()
        for current in frontier:
            out = subprocess.run(
                ["rg", "-l", "-F", Path(current).name, "--glob", "*.rs", "--glob", "*.wgsl",
                 str(Path(repo) / "crates")],
                capture_output=True, text=True).stdout
            for line in out.splitlines():
                rel = Path(line).resolve().relative_to(Path(repo).resolve()).as_posix()
                if rel in seen:
                    continue
                seen.add(rel)
                (nxt if rel.endswith(".wgsl") else found).add(rel)
        frontier = nxt
    return sorted(found)


def plan_for_paths(paths, repo, shader_users=None):
    """Map touched `paths` to a Plan. Never returns an implicit 'everything'."""
    shader_users = shader_users or (lambda p: default_shader_users(repo, p))
    plan = Plan()
    for path in sorted(set(paths)):
        if not is_gpu_path(path):
            continue
        plan.paths.append(path)
        if is_gltf_path(path):
            plan.glb = True
        for patterns, (filters, skips) in EXPLICIT_ROWS:
            if any(pat in path for pat in patterns):
                plan.filters.update(filters)
                plan.skips.update(skips)
        if path in BROAD_PATHS:
            plan.filters.update(BROAD_FILTERS)
            plan.broad.append((path, "affects every proof"))
            continue
        if path.startswith("crates/manifold-gpu/"):
            if path.endswith("raytrace.rs") or "/vulkan/" in path:
                continue  # rt row above / Vulkan not built here: smoke only
            plan.filters.update(BROAD_FILTERS)
            plan.broad.append((path, "manifold-gpu core"))
            continue
        if path.endswith(DOC_SUFFIXES):
            continue
        if path.endswith(".wgsl"):
            _map_wgsl(plan, path, repo, shader_users)
            continue
        if path.startswith(PROOFS_DIR) and path.endswith(".rs"):
            rel = path[len(PROOFS_DIR):].split("/")
            plan.filters.add(rel[0][:-3] + "::" if len(rel) == 1 else rel[0] + "::")
            continue
        if path in LIB_PROOF_ROWS:
            plan.filters.update(LIB_PROOF_ROWS[path])
            continue
        if path.startswith(RENDERER_SRC) and path.endswith(".rs"):
            plan.filters.update(path_attr_filters(path, repo) or module_filters(path))
            continue
        if is_gltf_path(path):
            continue
        if not path.startswith(("crates/manifold-renderer/", "crates/manifold-gpu/")):
            plan.notes.append(f"{path}: outside renderer/gpu crates, smoke only")
            continue
        plan.unmapped.append((path, "no GPU test mapping rule for this file type"))
    return plan


def _map_wgsl(plan, path, repo, shader_users):
    if not (Path(repo) / path).exists():
        plan.notes.append(f"{path}: deleted shader, smoke only")
        return
    if path.startswith(("crates/manifold-led/", "crates/manifold-recording/",
                        "crates/manifold-spectral/")):
        plan.notes.append(f"{path}: other crate's shader, smoke only")
        return
    if "/node_graph/freeze/shaders/" in path:
        plan.filters.add("freeze::")
        return
    users = shader_users(path)
    if not users:
        plan.unmapped.append((path, "no Rust file includes this shader; cannot find its proofs"))
        return
    if len(users) > SHARED_WGSL_USERS:
        plan.filters.update(BROAD_FILTERS)
        plan.broad.append((path, f"shared WGSL, {len(users)} users"))
        return
    for user in users:
        if user.startswith(RENDERER_SRC):
            plan.filters.update(module_filters(user))
        else:
            plan.notes.append(f"{path}: user {user} outside renderer")


def unmapped_message(plan):
    lines = ["GPU-PROOFS SCOPE: FAIL - touched GPU path(s) with no test mapping:"]
    lines += [f"  - {p}: {why}" for p, why in plan.unmapped]
    lines.append("Add a mapping rule in scripts/gpu_scope.py (EXPLICIT_ROWS or plan_for_paths) "
                 "and a case in scripts/test_gpu_scope.py. There is no run-everything fallback.")
    return "\n".join(lines)


if __name__ == "__main__":
    import sys
    p = plan_for_paths(sys.argv[1:], Path.cwd())
    print(p.describe())
    if p.unmapped:
        print(unmapped_message(p))
