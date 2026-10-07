#!/usr/bin/env python3
"""Changed Rust modules -> nextest filtersets. Full crate runs belong to nightly.

Source files select their module (including nested tests), existing sibling
*_tests modules and explicitly mapped integration binaries. Test files select
their integration binary. No reverse-dependent or whole-crate fallback.
"""

import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

def godfile_paths():
    """Read the source-of-truth CEILINGS table; reject unparsed entries."""
    source = Path(__file__).resolve().parent.parent / "crates/manifold-app/tests/godfile_regrowth.rs"
    text = re.sub(r"//[^\n]*", "", source.read_text())
    table = re.search(r"const CEILINGS\b[^=]*=\s*&\[(.*?)\];", text, re.S)
    entry = re.compile(r'\(\s*"([^"]+)"\s*,\s*\d[\d_]*\s*,?\s*\)\s*,?')
    if table is None or not entry.search(table[1]) or entry.sub("", table[1]).strip():
        raise ValueError(f"cannot parse CEILINGS in {source}")
    return entry.findall(table[1])


# Cross-file contracts: path -> (owning package, integration binaries).
INTEGRATION_ROWS = {
    "crates/manifold-renderer/src/node_graph/primitives/mod.rs": ("manifold-renderer", ["file_loader_exhaustiveness"]),
    "crates/manifold-renderer/src/node_graph/fluid.rs": ("manifold-renderer", ["fluid_preset"]),
    **{path: ("manifold-app", ["godfile_regrowth"]) for path in godfile_paths()},
}
# Contracts over every file under a prefix, Rust or not:
# (prefix, suffix, package, test modules, integration binaries).
PREFIX_ROWS = [
    # Scene-panel manifest rows are guarded by the existing INV-8 integration
    # test; keep it in the scoped CPU plan for every panel change.
    ("crates/manifold-ui/src/panels/", ".rs", "manifold-ui", [],
     ["no_bespoke_row_infra"]),
    # Bundled preset JSON is compiled into the renderer.
    ("crates/manifold-renderer/assets/", ".json", "manifold-renderer",
     ["node_graph::bundled_presets"], []),
    # The layout proofs scan every primitive's uniform mirror and hand shader.
    ("crates/manifold-renderer/src/node_graph/primitives/", ".rs", "manifold-renderer",
     [], ["uniform_layout_proof", "uniform_layout_extended"]),
    ("crates/manifold-renderer/src/node_graph/primitives/", ".wgsl", "manifold-renderer",
     [], ["uniform_layout_extended"]),
    # wgsl_validation parses every shader in the crate.
    ("crates/manifold-renderer/src/", ".wgsl", "manifold-renderer", [], ["wgsl_validation"]),
]
PATH_MOD = re.compile(r'#\[path\s*=\s*"([^"]+)"\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;')


def module_parts(relative):
    parts = list(relative.with_suffix("").parts)
    if parts[-1] in {"lib", "main", "mod"}:
        parts.pop()
    return parts


def module_name(source, root, aliases, seen=()):
    if source in seen:
        raise ValueError(f"cyclic #[path] module: {source}")
    if source in aliases:
        owner, name = aliases[source]
        return module_name(owner, root, aliases, (*seen, source)) + [name]
    return module_parts(source.relative_to(root))


@dataclass
class Plan:
    packages: set = field(default_factory=set)
    filters: set = field(default_factory=set)

    @property
    def filterset(self):
        return " | ".join(sorted(self.filters)) or "none()"

    def args(self):
        return [a for p in sorted(self.packages) for a in ("-p", p)] + ["-E", self.filterset]

    def describe(self):
        return "mode: scoped (changed modules + mapped integration binaries)\nfilterset: " + self.filterset


def plan_for_paths(paths, repo):
    repo, plan, cache = Path(repo), Plan(), {}
    for path in sorted(set(paths)):
        if path in INTEGRATION_ROWS:
            package, binaries = INTEGRATION_ROWS[path]
            plan.packages.add(package)
            plan.filters.update(f"(package(={package}) & binary(={binary}))" for binary in binaries)
        for prefix, suffix, package, modules, binaries in PREFIX_ROWS:
            if path.startswith(prefix) and path.endswith(suffix):
                plan.packages.add(package)
                plan.filters.update(f"(package(={package}) & test(/^{module}::/))" for module in modules)
                plan.filters.update(f"(package(={package}) & binary(={binary}))" for binary in binaries)
        parts = Path(path).parts
        if len(parts) < 4 or parts[0] != "crates" or not path.endswith(".rs"):
            continue
        crate = repo / parts[0] / parts[1]
        if not (crate / "Cargo.toml").exists():
            continue
        if crate not in cache:
            manifest = tomllib.loads((crate / "Cargo.toml").read_text())
            aliases = {}
            for source in (crate / "src").rglob("*.rs"):
                for target, name in PATH_MOD.findall(source.read_text()):
                    aliases[(source.parent / target).resolve()] = (source.resolve(), name)
            cache[crate] = manifest, aliases
        manifest, aliases = cache[crate]
        package = manifest["package"]["name"]
        plan.packages.add(package)
        binaries = set()
        if parts[2] == "tests":
            relative = Path(*parts[2:]).as_posix()
            explicit = [t["name"] for t in manifest.get("test", [])
                        if relative == t.get("path", "") or
                        (Path(t.get("path", "")).name in {"main.rs", "mod.rs"}
                         and relative.startswith(str(Path(t["path"]).parent) + "/"))]
            if explicit:
                binaries.update(explicit)
            elif (len(parts) == 4 or parts[4:] == ("main.rs",)
                  or (crate / "tests" / parts[3] / "main.rs").exists()):
                binaries.add(Path(parts[3]).stem)
            else:
                # Shared test code (tests/support/): no binary of its own, so
                # every top-level test that declares or #[path]s the directory.
                uses = re.compile(rf'\bmod\s+{re.escape(parts[3])}\b|"{re.escape(parts[3])}/')
                binaries.update(test.stem for test in (crate / "tests").glob("*.rs")
                                if uses.search(test.read_text()))
        elif parts[2] == "src":
            source, root = (repo / path).resolve(), (crate / "src").resolve()
            binary_filter = ""
            if len(parts) > 3 and parts[3] == "bin":
                target_path = Path(*parts[2:4], parts[4])
                if len(parts) > 5:
                    target_path /= "main.rs"
                target = next((b["name"] for b in manifest.get("bin", [])
                               if b.get("path") == target_path.as_posix()), Path(parts[4]).stem)
                binary_filter = f" & binary(={target})"
                root = (crate / target_path).parent.resolve()
                modules = [module_parts(source.relative_to(root)) if source != (crate / target_path).resolve() else []]
            else:
                modules = [module_name(source, root, aliases)]
            for sibling in source.parent.glob(source.stem + "_*tests.rs"):
                modules.append(module_name(sibling.resolve(), root, aliases))
            for module in modules:
                prefix = "::".join(module) + "::" if module else "tests::"
                plan.filters.add(f"(package(={package}){binary_filter} & test(/^{prefix}/))")
        for binary in binaries:
            plan.filters.add(f"(package(={package}) & binary(={binary}))")
    return plan


if __name__ == "__main__":
    import sys
    print(plan_for_paths(sys.argv[1:], Path.cwd()).describe())
