#!/usr/bin/env python3
"""Changed Rust modules -> nextest filtersets. Full crate runs belong to nightly.

Source files select their module (including nested tests), existing sibling
*_tests modules and explicitly mapped integration binaries. Test files select
their integration binary. No reverse-dependent or whole-crate fallback.
"""

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from gate_policy import godfile_paths, integration_rows, PREFIX_ROWS
from gate_policy import CATALOG_PATHS, CATALOG_PACKAGE
from gate_workspace import Workspace

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
    whole: set = field(default_factory=set)
    gpu_binaries: set = field(default_factory=set)

    @property
    def filterset(self):
        return " | ".join(sorted(self.filters)) or "none()"

    def args(self):
        return [a for p in sorted(self.packages) for a in ("-p", p)] + ["-E", self.filterset]

    def selections(self):
        return {package: f"package(={package})" if package in self.whole else
                " | ".join(sorted(f for f in self.filters if f"package(={package})" in f))
                for package in sorted(self.packages)}

    def describe(self):
        return "mode: scoped (changed modules + mapped integration binaries)\nfilterset: " + self.filterset


def plan_for_paths(paths, repo, workspace=None, base=None):
    repo, plan, cache = Path(repo).resolve(), Plan(), {}
    workspace = workspace or Workspace(repo)
    paths = sorted(set(paths))
    if hasattr(workspace, 'ownership_errors') and ((repo / 'Cargo.toml').is_file() or base):
        ownership = workspace.ownership_errors(paths, base)
        if ownership:
            raise ValueError('; '.join(ownership))
    rows = integration_rows()
    for path in sorted(set(paths)):
        if path in rows:
            package, binaries = rows[path]
            plan.packages.add(package)
            plan.filters.update(f"(package(={package}) & binary(={binary}))" for binary in binaries)
        for prefix, suffix, package, modules, binaries in PREFIX_ROWS:
            if path.startswith(prefix) and path.endswith(suffix):
                plan.packages.add(package)
                plan.filters.update(f"(package(={package}) & test(/^{module}::/))" for module in modules)
                plan.filters.update(f"(package(={package}) & binary(={binary}))" for binary in binaries)
        package = workspace.owner(path)
        if package and path == workspace.roots[package] + '/Cargo.toml':
            plan.packages.add(package)
            plan.whole.add(package)
        if not package or not path.endswith(".rs"):
            continue
        crate = repo / workspace.roots[package]
        relative = (repo / path).relative_to(crate)
        parts = ("crates", package, *relative.parts)
        # Deleted tests have no binary; a rename selects only its surviving path.
        if parts[2] == "tests" and not (repo / path).is_file():
            continue
        if crate not in cache:
            manifest = {"package": {"name": package}}
            for kind in ("test", "bin"):
                manifest[kind] = [{"name": t["name"], "path": Path(t["src_path"]).relative_to(crate).as_posix()}
                                  for t in workspace.targets(package, kind)]
            aliases = {}
            for source in (crate / "src").rglob("*.rs"):
                for target, name in PATH_MOD.findall(source.read_text()):
                    aliases[(source.parent / target).resolve()] = (source.resolve(), name)
            cache[crate] = manifest, aliases
        manifest, aliases = cache[crate]
        package = manifest["package"]["name"]
        plan.packages.add(package)
        target_sources = {Path(t['src_path']).resolve(): t for t in workspace.targets(package)}
        owning_target = target_sources.get((repo / path).resolve())
        if owning_target and ('lib' in owning_target['kind'] or 'custom-build' in owning_target['kind']):
            plan.whole.add(package)
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
        else:
            # A metadata target outside Cargo's conventional src/tests layout
            # changes the package; do not invent a path-derived module filter.
            plan.whole.add(package)
        for binary in binaries:
            plan.filters.add(f"(package(={package}) & binary(={binary}))")
    for expression in list(plan.filters):
        package = re.search(r'package\(=([^)]*)\)', expression)[1]
        binary = re.search(r'binary\(=([^)]*)\)', expression)
        if not binary or package not in workspace.packages:
            continue
        target = next((t for t in workspace.targets(package) if t['name'] == binary[1]), None)
        if target and 'gpu-proofs' in target.get('required-features', []):
            plan.gpu_binaries.add((package, binary[1]))
            plan.filters.remove(expression)
    plan.packages = plan.whole | {p for p in plan.packages if any(f'package(={p})' in f for f in plan.filters)}
    if any(path.startswith(CATALOG_PATHS) for path in paths):
        plan.packages.add(CATALOG_PACKAGE)
        plan.filters.add(f'(package(={CATALOG_PACKAGE}) & test(regenerates_in_sync))')
    if base:
        old = subprocess.run(['git', '-C', str(repo), 'ls-tree', '-r', '--name-only', base],
                             capture_output=True, text=True, check=True).stdout.splitlines()
        for package in plan.packages:
            if package not in workspace.roots:
                raise ValueError(f'unresolved test package: {package}')
            root = workspace.roots[package]
            sources = {p.relative_to(repo).as_posix() for p in (repo / root).rglob('*.rs')
                       if 'target' not in p.parts}
            touched = sources.intersection(paths)
            if root + '/Cargo.toml' not in old or (sources and len(touched) * 2 >= len(sources)):
                plan.whole.add(package)
    return plan


def validate_inventory(plan, package, listing):
    """A nonempty union must not conceal an ownership mapping typo."""
    suites = list(listing['rust-suites'].values())

    def matches(expression):
        binary = re.search(r'binary\(=([^)]*)\)', expression)
        prefix = re.search(r'test\(/(.*)/\)', expression)
        literal = re.search(r'test\(([^/)][^)]*)\)', expression)
        return {(s['binary-name'], name) for s in suites
                if not binary or s['binary-name'] == binary[1]
                for name in s['testcases'] if (not prefix or re.search(prefix[1], name))
                and (not literal or literal[1] in name)}

    selected = matches('')
    if not selected:
        raise ValueError(f'{package}: default-feature inventory contains no tests')
    if package not in plan.whole:
        for expression in sorted(plan.filters):
            if f'package(={package})' in expression and not matches(expression):
                raise ValueError(f'{package}: ownership mapping resolves to no tests: {expression}')
    return selected


if __name__ == "__main__":
    import sys
    print(plan_for_paths(sys.argv[1:], Path.cwd()).describe())
