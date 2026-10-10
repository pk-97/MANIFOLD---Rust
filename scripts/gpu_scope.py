#!/usr/bin/env python3
"""GPU-proofs scope selection: touched paths -> the focused set of GPU tests.

Single source for scripts/gpu_proofs_gate.py (default mode, dev and landing),
scripts/landing_gate.py and scripts/codex_checks.py. Rules:

- Every touched GPU path maps to test filters, plus the fixed SMOKE set.
- A path maps to the proofs of the thing it changes: NARROW_ROWS (clock, fields,
  domain nodes) beat the broad solver rows, and timing reporters (REPORTER_SKIPS)
  run only when their own file is touched or nightly.
- A GPU path with no mapping is a hard failure naming the path; the author adds
  a rule here. There is no run-everything fallback. Everything runs only with
  `gpu_proofs_gate.py --all` (nightly trunk_health.py).
- Measured duration never removes an owning proof from scoped runs.
- glb_conformance (the ~16-minute glTF sample sweep) runs only when glTF import
  paths are touched, and is exempt from the time budget.
- GPU backend core, shared WGSL and the proof harness map to BROAD, a bounded
  set (named below), never to everything.

Filters are libtest substring filters applied to the renderer lib binary
(primitive `gpu_tests`, freeze, graph runtime) and the `gpu_proofs` binary.

Obsolete when: the GPU test suite is fast enough to run whole at every landing.
"""

import json
import math
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

from gate_policy import (
    RENDERER_SRC, ENGINE_SRC, WATER_SRC, CONTRACT_TESTS_DIR, UI_PAINT_DIR, UI_PAINT_FILTERS,
    PROOFS_DIR, LANDING_BUDGET_S,
    SMOKE_FILTERS, RUNTIME_FILTERS, BROAD_FILTERS, SLOW_THRESHOLD_S, TIMES_PATH,
    GLB_TESTS, SHARED_WGSL_USERS, REPORTER_SKIPS, LIQUID_FORCE_FILTERS,
    LIQUID_DOMAIN_FILTERS, MATTER_DOMAIN_FILTERS, NARROW_ROWS, EXPLICIT_ROWS,
    BROAD_PATHS, GLTF_PATHS, DOC_SUFFIXES, PRESET_RUNTIME_DIR, LIB_PROOF_ROWS,
    GPU_BACKEND_ROOT, OTHER_SHADER_ROOTS, CATALOG_TEST_ROWS, CATALOG_PACKAGE, GPU_CONTRACT_TARGETS,
    GPU_FILTER_TARGETS, UI_PROJECTION_PATHS, is_inert_plan_path,
)
from gate_workspace import Workspace, module_mounts

SOURCE_ROOTS = (ENGINE_SRC, WATER_SRC, RENDERER_SRC)


def source_root(path):
    return next((root for root in SOURCE_ROOTS if path.startswith(root)), RENDERER_SRC)


def learned_times_path():
    repo = TIMES_PATH.parent.parent
    try:
        result = subprocess.run(["git", "-C", str(repo), "rev-parse", "--git-common-dir"],
                                capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError) as error:
        print(f"[WARN] cannot locate shared GPU timings: {error}", file=sys.stderr)
        return None
    if result.returncode:
        return None
    return (repo / result.stdout.strip()).resolve() / "gpu-test-times.json"


def read_times(path):
    """{test name: seconds} from the measured-times file; {} if absent."""
    path = Path(path)
    if not path.exists():
        return {}
    times = json.loads(path.read_text())["tests"]
    if not isinstance(times, dict):
        raise ValueError(f"invalid GPU measurements in {path}")
    times = {n: v["s"] if isinstance(v, dict) else v for n, v in times.items()}
    if not isinstance(times, dict) or any(
            not isinstance(n, str) or not isinstance(s, (int, float))
            or isinstance(s, bool) or not math.isfinite(s) or s < 0
            for n, s in times.items()):
        raise ValueError(f"invalid GPU measurements in {path}")
    return times


def merge_times(*tables):
    merged = {}
    for times in tables:
        merged.update(times)
    return merged


def load_times(path=None):
    if path is not None:
        return read_times(path)
    times = read_times(TIMES_PATH)
    learned = learned_times_path()
    if learned is not None:
        try:
            times = merge_times(times, read_times(learned))
        except (OSError, ValueError, KeyError, TypeError) as error:
            print(f"[WARN] GPU timing cache unreadable; using committed timings: {error}",
                  file=sys.stderr)
    return times


def slow_tests(times=None):
    """[(name, seconds)] measured over SLOW_THRESHOLD_S, slowest first."""
    times = load_times() if times is None else times
    return sorted(((n, s) for n, s in times.items()
                   if s > SLOW_THRESHOLD_S and n not in GLB_TESTS),
                  key=lambda t: -t[1])


_CPU_PLAN_UNSET = object()


def is_gpu_path(path, workspace=None):
    """Paths that trigger the GPU-proofs leg (mirrors the context-nudge triggers)."""
    if is_inert_plan_path(path):
        return False
    if path.endswith('.rs') and any(
            path == prefix or (prefix.endswith('/') and path.startswith(prefix))
            for prefix in UI_PROJECTION_PATHS):
        return False
    if workspace:
        owner = workspace.owner(path)
        if owner and 'gpu-proofs' in workspace.packages[owner]['features']:
            source = (workspace.repo / path).resolve()
            for target in workspace.targets(owner, 'test'):
                root = Path(target['src_path']).resolve()
                if ('gpu-proofs' in target.get('required-features', [])
                        and root.name == 'main.rs' and source.is_relative_to(root.parent)):
                    return True
                if target['name'] == GPU_CONTRACT_TARGETS.get(owner) and source == root:
                    return True
        if (owner and 'gpu-proofs' in workspace.packages[owner]['features']
                and ((path.startswith(workspace.roots[owner] + '/src/')
                      and path.endswith(('.rs', '.wgsl')))
                     or path == workspace.roots[owner] + '/Cargo.toml')):
            return True
    if path.endswith(".wgsl"):
        return True
    if path.startswith((GPU_BACKEND_ROOT, UI_PAINT_DIR, ENGINE_SRC, WATER_SRC, *CONTRACT_TESTS_DIR, RENDERER_SRC + "node_graph/")):
        return True
    if "shaders/" in path or "gpu::gpu_encoder" in path:
        return True
    if path.startswith(PRESET_RUNTIME_DIR) or path in LIB_PROOF_ROWS:
        return True
    return "tests/gpu_proofs/" in path or is_gltf_path(path)


def is_gltf_path(path):
    return any(path.startswith(p) for p in GLTF_PATHS)


def glb_conformance_route(workspace):
    """Discover the standalone target or its module in a folded GPU test root."""
    from crate_move_replay import module_items
    routes = set()
    for package in workspace.feature_packages('gpu-proofs'):
        for target in workspace.targets(package, 'test'):
            if (target['name'] == 'glb_conformance'
                    or Path(target['src_path']).name == 'glb_conformance.rs'):
                routes.add((package, target['name'], ''))
                continue
            if 'gpu-proofs' not in target.get('required-features', []):
                continue
            source = Path(target['src_path'])
            if not source.is_file():
                continue
            text = source.read_text()
            for start, end, head, scope in module_items(text):
                declaration = re.fullmatch(r'(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;', text[head:end])
                if not declaration:
                    continue
                name = declaration[1]
                attrs = re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', text[start:head])
                filename = Path(attrs[-1]).name if attrs else name + '.rs'
                if filename == 'glb_conformance.rs':
                    routes.add((package, target['name'], '::'.join((*scope, name)) + '::'))
    if len(routes) > 1:
        raise ValueError('ambiguous glb_conformance GPU target: ' + repr(sorted(routes)))
    return next(iter(routes), None)


@dataclass
class Plan:
    paths: list = field(default_factory=list)       # GPU paths considered
    filters: set = field(default_factory=set)
    skips: set = field(default_factory=set)
    ui_paint: bool = False
    glb: bool = False
    broad: list = field(default_factory=list)        # (path, reason) that mapped to BROAD
    unmapped: list = field(default_factory=list)     # (path, why)
    notes: list = field(default_factory=list)
    workspace: object = None
    required_binaries: set = field(default_factory=set)
    whole_packages: set = field(default_factory=set)

    @property
    def active(self):
        return bool(self.paths)

    def final_filters(self):
        return sorted(set(SMOKE_FILTERS) | self.filters)

    def final_skips(self):
        # Timing never removes an owning proof. Exact selections also override
        # the explicit reporter-only exclusions in the semantic policy.
        return sorted(s for s in self.skips if not any(s in f for f in self.filters))

    def deferred(self):
        return []

    def runs(self):
        if not self.active:
            return []
        if self.workspace is None:
            raise ValueError('GPU plan has no Cargo ownership inventory')
        route = glb_conformance_route(self.workspace)
        if self.glb and route is None:
            raise ValueError('no glb_conformance GPU target in Cargo inventory')
        runs = []
        for package in self.workspace.feature_packages('gpu-proofs'):
            filters = (UI_PAINT_FILTERS if self.ui_paint and self.workspace.owner(UI_PAINT_DIR) == package
                       else self.final_filters())
            if package in GPU_CONTRACT_TARGETS:
                filters = [re.sub(r'^(exec|freeze|load|runtime|water|palette|preview_encoding|primitive_registry|node_graph)::',
                                  r'contracts::\1::', value) for value in filters]
            targets = [t['name'] for t in self.workspace.targets(package, 'test')
                       if ('gpu-proofs' in t.get('required-features', [])
                           or (self.ui_paint and self.workspace.owner(UI_PAINT_DIR) == package
                               and t['name'] == 'main')
                           or t['name'] == GPU_CONTRACT_TARGETS.get(package))
                       and t['name'] not in GLB_TESTS
                       and not (route and route[:2] == (package, t['name']) and not route[2])]
            has_lib = bool(self.workspace.targets(package, 'lib'))
            if has_lib:
                runs.append({'package': package, 'targets': [], 'lib': True, 'target': 'lib',
                             'filters': [] if package in self.whole_packages else filters,
                             'skips': self.final_skips(), 'budgeted': True})
            for target in targets:
                whole = package in self.whole_packages or (package, target) in self.required_binaries
                skips = [] if whole else self.final_skips()
                if route and route[:2] == (package, target) and route[2]:
                    # The folded sweep retains its separate, unbudgeted run.
                    skips = sorted(set(skips) | {route[2]})
                runs.append({'package': package, 'targets': [target], 'lib': False, 'target': target,
                             'filters': [] if whole else filters,
                             'skips': skips, 'budgeted': True})
        if self.glb:
            package, target, prefix = route
            runs.append({'package': package, 'targets': [target], 'lib': False, 'target': target,
                         'filters': [prefix] if prefix else [], 'skips': [], 'budgeted': False})
        available = {(run['package'], run['target']) for run in runs}
        for name in self.final_filters():
            owner = GPU_FILTER_TARGETS.get(name)
            if owner is not None and owner not in available:
                raise ValueError(f'GPU filter {name!r} has no runnable Cargo owner: {owner[0]}/{owner[1]}')
        # Unknown filters keep their current coverage. Whole selections and
        # the folded glTF harness retain their independent selection rules.
        return [run for run in runs
                if not run['filters'] or (route and route[:2] == (run['package'], run['target']))
                or any(name not in GPU_FILTER_TARGETS
                       or GPU_FILTER_TARGETS[name] == (run['package'], run['target'])
                       for name in run['filters'])]

    def describe(self):
        lines = [f"{len(self.paths)} GPU path(s) touched; smoke + mapped filters"]
        lines.append(f"  filters: {', '.join(self.final_filters())}")
        if self.whole_packages:
            lines.append("  whole packages (filters do not limit these): " +
                         ", ".join(sorted(self.whole_packages)))
        if self.required_binaries:
            lines.append("  whole test binaries: " + ", ".join(
                f"{package}/{target}" for package, target in sorted(self.required_binaries)))
        skips = [s for s in self.final_skips()
                 if any(f in s or s in f for f in self.final_filters())]
        if skips:
            lines.append(f"  skips: {', '.join(skips)}")
        if self.deferred():
            lines.append("GPU-PROOFS DEFERRED: " + ", ".join(
                f"{name} ({secs:.0f}s)" for name, secs in self.deferred()))
        if self.ui_paint:
            lines.append("  UI paint proofs: " + ", ".join(UI_PAINT_FILTERS))
        if self.broad:
            lines.append("  broad set (runtime + lighting) because: " +
                         "; ".join(f"{p} ({why})" for p, why in self.broad))
        if self.glb:
            lines.append("  glb_conformance: glTF import paths touched (exempt from time budget)")
        return "\n".join(lines)


def module_filters(path, root=None):
    """Test module filters for a source file relative to its Cargo source root."""
    root = root or source_root(path)
    parts = path[len(root):].split("/")
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


def contract_module_filters(path, repo):
    """Resolve relocated contracts through their real Rust module mounts."""
    repo = Path(repo)
    target = (repo / path).resolve()
    roots = (repo / RENDERER_SRC / "lib.rs",
             repo / "crates/manifold-nodes/tests/main.rs",
             repo / "crates/manifold-app/tests/renderer_contracts.rs")
    return sorted({"::".join(prefix) + "::" for root in roots
                   for prefix in module_mounts(root).get(target, ())})


def path_attr_filters(path, repo):
    """Filters for a `<dir>/tests/<file>.rs` pulled in by `#[path] mod x;` in `<dir>/mod.rs`.

    The test module is named by that declaration, not by the file path, so the
    path-derived filter would select nothing. Unresolvable preset_runtime test
    files fall back to the whole preset_runtime module rather than to nothing.
    """
    if path.startswith(CONTRACT_TESTS_DIR):
        return contract_module_filters(path, repo)
    root = source_root(path)
    parts = path[len(root):].split("/")
    if len(parts) < 3 or parts[-2] != "tests":
        return None
    dirs = parts[:-2]
    try:
        text = (Path(repo) / root / "/".join(dirs) / "mod.rs").read_text()
    except OSError:
        text = ""
    from crate_move_replay import module_items
    for start, end, head, scope in module_items(text):
        declaration = re.fullmatch(r'(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;', text[head:end])
        attrs = re.findall(r'#\[path\s*=\s*"tests/([^"\n]+)"\]', text[start:head])
        if declaration and attrs and attrs[-1] == parts[-1]:
            return ["::".join((*dirs, *scope, declaration[1])) + "::"]
    if path.startswith(PRESET_RUNTIME_DIR):
        return ["runtime::"]
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


def shader_index(repo, workspace):
    """Read each source once for a planning run, including transitive WGSL users."""
    users = {}
    sources = (source for root in workspace.roots.values()
               for source in (Path(repo) / root).rglob('*'))
    for source in sources:
        if source.suffix not in {'.rs', '.wgsl'} or not source.is_file():
            continue
        relative = source.relative_to(repo).as_posix()
        for name in set(re.findall(r'[\w.-]+\.wgsl', source.read_text())):
            users.setdefault(name, set()).add(relative)

    def resolve(path):
        found, frontier, seen = set(), {path}, {path}
        while frontier:
            following = set()
            for current in frontier:
                for user in users.get(Path(current).name, ()):
                    if user in seen:
                        continue
                    seen.add(user)
                    (following if user.endswith('.wgsl') else found).add(user)
            frontier = following
        return sorted(found)
    return resolve


def proof_module_prefix(path, repo, root=None):
    """Honor explicit catalog-proof mounts before deriving a path prefix."""
    root = Path(root) if root is not None else Path(repo) / PROOFS_DIR / 'main.rs'
    if root.is_file():
        from crate_move_replay import module_items
        text = root.read_text()
        for start, end, head, scope in module_items(text):
            declaration = re.fullmatch(r'(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;', text[head:end])
            attrs = re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', text[start:head])
            if declaration and attrs and root.parent.joinpath(*scope, attrs[-1]).resolve() == (Path(repo) / path).resolve():
                return '::'.join((*scope, declaration[1])) + '::'
    parts = list((Path(repo) / path).resolve().relative_to(root.parent.resolve()).with_suffix('').parts)
    return '::'.join(parts[:-1] if parts[-1] == 'mod' else parts) + '::'


def changed_test_filters(path, repo, base, patch=None):
    """Promote changed test bodies; shared-helper edits retain module scope."""
    # Only renderer lib and proof paths have a derivable test-name prefix.
    if not path.startswith((*SOURCE_ROOTS, PROOFS_DIR)):
        return set()
    source = Path(repo) / path
    if source.suffix != ".rs" or not source.exists():
        return set()
    from crate_move_replay import production_text
    text = production_text(source.read_text())
    if "#[test]" not in text:
        return set()
    diff = None if patch is not None else subprocess.run(["git", "-C", str(repo), "diff", "--no-ext-diff",
                           "--no-textconv", "-U0", "--merge-base", base, "--", path],
                          capture_output=True, text=True)
    if diff is not None and diff.returncode:
        raise RuntimeError(f"cannot scope changed test bodies: {diff.stderr.strip()}")
    patch = diff.stdout if diff is not None else patch
    hunks = [(int(m[1]), max(1, int(m[2] or 1))) for m in re.finditer(
        r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", patch, re.M)]
    if path.startswith(PROOFS_DIR):
        prefix = proof_module_prefix(path, repo)
    else:
        prefixes = path_attr_filters(path, repo) or module_filters(path)
        prefix = prefixes[0] if prefixes else ''
    selected = set()
    for match in re.finditer(r"#\[test\]\s*(?:#\[[^\n]+\]\s*)*"
                             r"fn (?P<name>\w+)\([^)]*\)[^{;]*\{", text):
        start = text.count("\n", 0, match.end()) + 1
        # Rustfmt puts a function's closing brace at the fn's indentation.
        fn_line = text.rfind("\n", 0, text.index("fn ", match.start())) + 1
        indent = re.match(r"[ \t]*", text[fn_line:])[0]
        end = re.search(r"^" + indent + r"\}", text[match.end():], re.M)
        stop = start + text[match.end():match.end() + end.end()].count("\n") if end else start
        if not any(row <= stop and row + count - 1 >= start for row, count in hunks):
            continue
        modules = []
        for mod in re.finditer(r"^([ \t]*)mod (\w+) \{", text[:match.start()], re.M):
            close = re.search(r"^" + mod[1] + r"\}", text[mod.end():], re.M)
            if close is None or mod.end() + close.start() > match.start():
                modules.append(mod[2])
        selected.add(prefix + "::".join(modules + [match["name"]]))
    return selected


def plan_for_paths(paths, repo, shader_users=None, base="origin/main", workspace=None,
                   cpu_plan=_CPU_PLAN_UNSET):
    """Map touched `paths` to a Plan. Never returns an implicit 'everything'."""
    paths = [path for path in paths if not is_inert_plan_path(path)]
    workspace = workspace or Workspace(repo)
    if shader_users is None:
        shader_users = shader_index(repo, workspace) if any(p.endswith('.wgsl') for p in paths) else lambda p: []
    plan = Plan(workspace=workspace)
    # Retired crates have no runnable target. Only skip paths Git confirms
    # were deleted; moved destinations are independently scoped from the diff.
    unowned_missing = [p for p in paths if workspace.owner(p) is None
                       and not (Path(repo) / p).exists()]
    retired = set()
    if unowned_missing and (Path(repo) / '.git').exists():
        deleted = subprocess.run(
            ['git', '-C', str(repo), 'diff', '--no-renames', '--diff-filter=D',
             '--name-only', '--merge-base', base, '--', *unowned_missing],
            capture_output=True, text=True)
        if deleted.returncode:
            raise RuntimeError(f'cannot scope deleted crate paths: {deleted.stderr.strip()}')
        retired.update(deleted.stdout.splitlines())
    test_paths = [p for p in paths if p.endswith('.rs') and (Path(repo) / p).is_file()
                  and '#[test]' in (Path(repo) / p).read_text()
                  and p.startswith((*SOURCE_ROOTS, PROOFS_DIR))]
    patches = {}
    if test_paths:
        diff = subprocess.run(['git', '-C', str(repo), 'diff', '--no-ext-diff',
                               '--no-textconv', '--no-renames', '-U0', '--merge-base', base,
                               '--', *test_paths], capture_output=True, text=True)
        if diff.returncode:
            raise RuntimeError(f'cannot scope changed test bodies: {diff.stderr.strip()}')
        for section in re.split(r'^diff --git ', diff.stdout, flags=re.M)[1:]:
            path = re.search(r'^\+\+\+ b/(.+)$', section, re.M)
            if path:
                patches[path[1]] = section
    for path in sorted(set(paths)):
        if path in retired:
            continue
        if not is_gpu_path(path, workspace):
            continue
        catalog_mapped = path.endswith(".rs") and any(
            path.startswith(prefix) for prefix, _, _ in CATALOG_TEST_ROWS)
        plan.filters.update("node_graph::catalog_tests::" + module + "::"
                            for prefix, module, _ in CATALOG_TEST_ROWS
                            if path.startswith(prefix) and path.endswith(".rs"))
        plan.paths.append(path)
        owner = workspace.owner(path)
        if owner in GPU_CONTRACT_TARGETS and any(
                target['name'] == GPU_CONTRACT_TARGETS[owner]
                and (Path(repo) / path).resolve() == Path(target['src_path']).resolve()
                for target in workspace.targets(owner, 'test')):
            plan.required_binaries.add((owner, GPU_CONTRACT_TARGETS[owner]))
            continue
        if owner and (path == workspace.roots[owner] + '/Cargo.toml'
                      or path == WATER_SRC + 'lib.rs'):
            plan.whole_packages.add(owner)
            continue
        if path.startswith(UI_PAINT_DIR):
            plan.ui_paint = True
            continue
        if path.startswith(CONTRACT_TESTS_DIR):
            mounted = contract_module_filters(path, repo)
            if not mounted:
                plan.unmapped.append((path, "contract test has no resolvable Rust module mount"))
                continue
            plan.filters.update(mounted)
        plan.filters.update(changed_test_filters(path, repo, base, patches.get(path, '')))
        if is_gltf_path(path):
            plan.glb = True
        # A path that several features own maps to every one of their rows.
        narrow = [(("",), row) for pats, row in NARROW_ROWS
                  if any(pat in path for pat in pats)]
        for patterns, (filters, skips) in (narrow or EXPLICIT_ROWS):
            if any(pat in path for pat in patterns):
                plan.filters.update(filters)
                plan.skips.update(skips)
        if path in BROAD_PATHS:
            plan.filters.update(BROAD_FILTERS)
            plan.broad.append((path, "affects every proof"))
            continue
        if path.startswith(GPU_BACKEND_ROOT):
            plan.whole_packages.add(owner)
            plan.ui_paint = True
            if path.endswith("raytrace.rs") or "/vulkan/" in path:
                continue  # rt row above / Vulkan not built here: smoke only
            plan.filters.update(BROAD_FILTERS)
            plan.broad.append((path, "GPU backend core"))
            continue
        if path in LIB_PROOF_ROWS:
            plan.filters.update(LIB_PROOF_ROWS[path])
            continue
        if path.endswith(DOC_SUFFIXES):
            continue
        if path.endswith(".wgsl"):
            _map_wgsl(plan, path, repo, shader_users)
            continue
        if path.startswith(PROOFS_DIR) and path.endswith(".rs"):
            prefix = proof_module_prefix(path, repo)
            plan.filters.add(prefix.split("::")[0] + "::")
            continue
        # A moved proof target keeps its module scope under its Cargo owner.
        if owner and path.endswith(".rs"):
            proof_targets = [
                target for target in workspace.targets(owner, "test")
                if "gpu-proofs" in target.get("required-features", [])
                and (Path(repo) / path).resolve().is_relative_to(Path(target["src_path"]).resolve().parent)
                and Path(target["src_path"]).name == "main.rs"
            ]
            if proof_targets:
                for target in proof_targets:
                    root = Path(target["src_path"])
                    if (Path(repo) / path).resolve() == root.resolve():
                        plan.required_binaries.add((owner, target["name"]))
                    else:
                        prefix = proof_module_prefix(path, repo, root)
                        plan.filters.add(prefix.split("::")[0] + "::")
                continue
        if path.startswith(SOURCE_ROOTS) and path.endswith(".rs"):
            plan.filters.update(path_attr_filters(path, repo) or module_filters(path))
            continue
        if path.startswith(CONTRACT_TESTS_DIR):
            continue  # Its real consolidated-harness mount was resolved above.
        if is_gltf_path(path):
            continue
        owner = workspace.owner(path)
        if owner and path.endswith('.rs'):
            root = workspace.roots[owner] + '/src/'
            if path.startswith(root):
                if catalog_mapped:
                    plan.filters.update(module_filters(path, root))
                    continue
                plan.whole_packages.add(owner)
                continue
        plan.unmapped.append((path, "no GPU test mapping rule for this file type"))
    # Feature-gated integration targets selected by CPU ownership belong here.
    if cpu_plan is _CPU_PLAN_UNSET:
        import cpu_scope
        # Nested CPU ownership must use the same base as GPU deletion scope.
        # Metadata-only fixtures have no Git history to consult.
        cpu_plan = cpu_scope.plan_for_paths(
            paths, repo, workspace, base=base if (Path(repo) / '.git').exists() else None)
    if cpu_plan is not None:
        plan.required_binaries.update(cpu_plan.gpu_binaries)
        plan.filters.update(cpu_plan.gpu_filters)
    return plan


def _map_wgsl(plan, path, repo, shader_users):
    if not (Path(repo) / path).exists():
        owner = plan.workspace.owner(path)
        if owner in plan.workspace.feature_packages('gpu-proofs'):
            plan.whole_packages.add(owner)
            plan.notes.append(f'{path}: deleted shader; every owning package proof is required')
        else:
            plan.unmapped.append((path, 'deleted shader has no proof package owner'))
        return
    if path.startswith(ENGINE_SRC + "freeze/shaders/"):
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
        if user.startswith(SOURCE_ROOTS):
            plan.filters.update(LIB_PROOF_ROWS.get(user, module_filters(user)))
        else:
            owner = plan.workspace.owner(user)
            if owner in plan.workspace.feature_packages('gpu-proofs'):
                plan.whole_packages.add(owner)
            else:
                plan.unmapped.append((path, f'shader user {user} has no proof package owner'))


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
