#!/usr/bin/env python3
"""Changed Rust modules -> nextest filtersets. Full crate runs belong to nightly.

Source files select their module (including nested tests), existing sibling
*_tests modules and explicitly mapped integration binaries. Private test files
select their mounted module; shared helpers select their integration binary.
Empty path-derived selections widen to their owning binary or package after
compiled inventory validation; explicit mappings fail closed.
"""

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from gate_policy import godfile_paths, integration_rows, PREFIX_ROWS, is_inert_plan_path
from gate_policy import CATALOG_PATHS, CATALOG_PACKAGE
from gate_workspace import Workspace, module_mounts

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
    gpu_filters: set = field(default_factory=set)
    path_filters: dict = field(default_factory=dict)
    widening_reasons: set = field(default_factory=set)

    @property
    def filterset(self):
        return " | ".join(sorted(self.selections().values())) or "none()"

    def args(self):
        return [a for p in sorted(self.packages) for a in ("-p", p)] + ["-E", self.filterset]

    def selections(self):
        return {package: f"package(={package})" if package in self.whole else
                " | ".join(sorted(f for f in self.filters if f"package(={package})" in f))
                for package in sorted(self.packages)}

    def describe(self):
        return "\n".join(["mode: scoped (changed modules + mapped integration binaries)",
                          "filterset: " + self.filterset, *sorted(self.widening_reasons)])


GPU_PROOF_TESTS = re.compile(r'#\[cfg\(all\(test,\s*feature\s*=\s*"gpu-proofs"\)\)\]')


def gpu_proofs_only(source):
    """True when every test module in `source` is built only under gpu-proofs.
    A deleted or moved file has no text and keeps its module filter."""
    if not source.is_file():
        return False
    text = source.read_text()
    return bool(GPU_PROOF_TESTS.search(text)) and '#[cfg(test)]' not in text


def test_module_prefixes(source, prefixes):
    """Keep helper callers covered when narrowing a folded test target."""
    from crate_move_replay import code_mask
    text = code_mask(source.read_text())
    # Without tests of its own this can be shared setup. Public exports and
    # opaque inclusions can affect callers anywhere in the binary.
    if (not re.search(r'#\[\s*test\s*\]', text)
            or re.search(r'\bpub\b(?!\s*\(\s*super\s*\))|\bmacro_export\b|\b(?:include|macro_rules)\s*!', text)):
        return {()}
    if re.search(r'\bpub\s*\(\s*super\s*\)', text):
        return {prefix[:-1] for prefix in prefixes}
    return prefixes


def plan_for_paths(paths, repo, workspace=None, base=None):
    repo, plan, cache = Path(repo).resolve(), Plan(), {}
    workspace = workspace or Workspace(repo)
    paths = sorted(path for path in set(paths) if not is_inert_plan_path(path))
    if hasattr(workspace, 'ownership_errors') and ((repo / 'Cargo.toml').is_file() or base):
        ownership = workspace.ownership_errors(paths, base)
        if ownership:
            raise ValueError('; '.join(ownership))
    mounts = {}
    rows = integration_rows()
    explicit_filters = set()
    for path in sorted(set(paths)):
        if path in rows:
            package, binaries = rows[path]
            plan.packages.add(package)
            plan.filters.update(f"(package(={package}) & binary(={binary}))" for binary in binaries)
        for prefix, suffix, package, modules, binaries in PREFIX_ROWS:
            if path.startswith(prefix) and path.endswith(suffix):
                plan.packages.add(package)
                expressions = {f"(package(={package}) & test(/^{module}::/))" for module in modules}
                plan.filters.update(expressions)
                explicit_filters.update(expressions)
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
        # Deleted test/bin targets select only their surviving destination.
        if (parts[2] == "tests" or parts[2:4] == ("src", "bin")) and not (repo / path).is_file():
            continue
        if crate not in cache:
            manifest = {"package": {"name": package}}
            for kind in ("test", "bin"):
                manifest[kind] = [{"name": t["name"], "path": Path(t["src_path"]).relative_to(crate).as_posix()}
                                  for t in workspace.targets(package, kind)]
            aliases = {}
            from crate_move_replay import module_items
            for source in (crate / "src").rglob("*.rs"):
                text = source.read_text()
                for start, end, head, _scope in module_items(text):
                    declaration = re.fullmatch(r'(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;',
                                                text[head:end])
                    attrs = re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', text[start:head])
                    if declaration and attrs:
                        aliases[(source.parent / attrs[-1]).resolve()] = (source.resolve(), declaration[1])
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
            source = (repo / path).resolve()
            explicit = False
            for target in manifest.get("test", []):
                target_source = (crate / target["path"]).resolve()
                if target_source not in mounts:
                    mounts[target_source] = module_mounts(target_source)
                prefixes = mounts[target_source].get(source)
                if prefixes:
                    explicit = True
                    for prefix in test_module_prefixes(source, prefixes):
                        if not prefix:
                            binaries.add(target["name"])
                            continue
                        prefix = "::".join(prefix) + "::"
                        expression = (f"(package(={package}) & binary(={target['name']})"
                                      f" & test(/^{prefix}/))")
                        plan.filters.add(expression)
                        plan.path_filters.setdefault(expression, set()).add(path)
                elif (source == target_source or
                      (target_source.name in {"main.rs", "mod.rs"}
                       and source.is_relative_to(target_source.parent))):
                    explicit = True
                    binaries.add(target["name"])
            if not explicit and (len(parts) == 4 or parts[4:] == ("main.rs",)
                  or (crate / "tests" / parts[3] / "main.rs").exists()):
                binaries.add(Path(parts[3]).stem)
            elif not explicit:
                # Shared test code (tests/support/): no binary of its own, so
                # every top-level test that declares or #[path]s the directory.
                from crate_move_replay import production_text
                uses = re.compile(rf'\bmod\s+{re.escape(parts[3])}\b|"{re.escape(parts[3])}/')
                binaries.update(test.stem for test in (crate / "tests").glob("*.rs")
                                if uses.search(production_text(test.read_text())))
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
            elif gpu_proofs_only(source):
                # Its tests run in the gpu-proofs leg (gpu_scope); a CPU
                # filter here would select nothing and fail ownership.
                modules = []
            else:
                modules = [module_name(source, root, aliases)]
            for sibling in source.parent.glob(source.stem + "_*tests.rs"):
                modules.append(module_name(sibling.resolve(), root, aliases))
            for module in modules:
                prefix = "::".join(module) + "::" if module else "tests::"
                expression = f"(package(={package}){binary_filter} & test(/^{prefix}/))"
                plan.filters.add(expression)
                plan.path_filters.setdefault(expression, set()).add(path)
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
            module = re.search(r'test\(/\^(.*)/\)', expression)
            if module:
                plan.gpu_filters.add(module[1])
            else:
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
    plan.path_filters = {expression: sources for expression, sources in plan.path_filters.items()
                         if expression in plan.filters and expression not in explicit_filters}
    return plan


def validate_inventory(plan, package, listing):
    """Widen empty source modules, but keep explicit ownership mappings fail-closed."""
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
        missing_paths = set()
        missing_binaries = set()
        for expression in sorted(plan.filters):
            if f'package(={package})' in expression and not matches(expression):
                if expression not in plan.path_filters:
                    raise ValueError(f'{package}: ownership mapping resolves to no tests: {expression}')
                binary = re.search(r'binary\(=([^)]*)\)', expression)
                if binary:
                    whole_binary = f"(package(={package}) & binary(={binary[1]}))"
                    if not matches(whole_binary):
                        raise ValueError(f'{package}: owning binary has no tests: {binary[1]}')
                    missing_binaries.add(whole_binary)
                    plan.widening_reasons.add(
                        f'{binary[1]} has an empty module selection: running the whole binary')
                else:
                    missing_paths.update(plan.path_filters[expression])
        plan.filters.update(missing_binaries)
        if missing_paths:
            plan.whole.add(package)
            plan.widening_reasons.update(
                f'{Path(path).name} has no tests of its own: running {package} whole'
                for path in missing_paths)
    return selected


if __name__ == "__main__":
    import sys
    print(plan_for_paths(sys.argv[1:], Path.cwd()).describe())
