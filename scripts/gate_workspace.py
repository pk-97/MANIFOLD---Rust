#!/usr/bin/env python3
"""Cargo's package, target and feature inventory. Resolution errors are red."""
import json
import os
from pathlib import Path
import subprocess
import re


class Workspace:
    def __init__(self, repo, metadata=None):
        self.repo = Path(repo).resolve()
        if metadata is None:
            command = ['cargo', 'metadata', '--no-deps', '--format-version', '1',
                       '--manifest-path', str(self.repo / 'Cargo.toml')]
            result = subprocess.run(command, capture_output=True, text=True, timeout=30,
                                    env=dict(os.environ, CARGO_BUILD_JOBS='4', RUSTC_WRAPPER=''))
            if result.returncode:
                raise ValueError('cargo metadata failed: ' + result.stderr.strip())
            metadata = json.loads(result.stdout)
        self._validate_metadata(metadata)
        members = set(metadata['workspace_members'])
        self.packages = {p['name']: p for p in metadata['packages'] if p['id'] in members}
        package_ids = {p['id'] for p in metadata['packages']}
        if not members <= package_ids:
            missing = sorted(members - package_ids)
            raise ValueError(f'cargo metadata workspace member missing package: {missing}')
        if not self.packages:
            raise ValueError('cargo metadata returned no workspace packages')
        self.roots = {name: Path(p['manifest_path']).resolve().parent.relative_to(self.repo).as_posix()
                      for name, p in self.packages.items()}
        if len(self.roots) != len(set(self.roots.values())):
            raise ValueError('cargo metadata has duplicate workspace package roots')

    @staticmethod
    def _validate_metadata(metadata):
        if not isinstance(metadata, dict):
            raise ValueError('cargo metadata must be an object')
        members = metadata.get('workspace_members')
        packages = metadata.get('packages')
        if (not isinstance(members, list) or any(not isinstance(item, str) for item in members)
                or not isinstance(packages, list)):
            raise ValueError('cargo metadata has malformed workspace membership')
        for package in packages:
            if not isinstance(package, dict):
                raise ValueError('cargo metadata has a malformed package')
            for key in ('id', 'name', 'manifest_path', 'dependencies', 'features', 'targets'):
                if key not in package:
                    raise ValueError(f'cargo metadata package missing {key}')
            if (not isinstance(package['id'], str) or not isinstance(package['name'], str)
                    or not isinstance(package['manifest_path'], str)
                    or not isinstance(package['dependencies'], list)
                    or not isinstance(package['features'], dict)
                    or not isinstance(package['targets'], list)):
                raise ValueError(f"cargo metadata package {package.get('name', '?')} has malformed fields")
            for dependency in package['dependencies']:
                if not isinstance(dependency, dict) or not isinstance(dependency.get('name'), str):
                    raise ValueError(f"cargo metadata package {package['name']} has malformed dependency")
            for target in package['targets']:
                if (not isinstance(target, dict) or not isinstance(target.get('name'), str)
                        or not isinstance(target.get('kind'), list)
                        or not isinstance(target.get('src_path'), str)):
                    raise ValueError(f"cargo metadata package {package['name']} has malformed target")
                if any(not isinstance(kind, str) for kind in target['kind']):
                    raise ValueError(f"cargo metadata package {package['name']} has malformed target kind")
                required = target.get('required-features', [])
                if (not isinstance(required, list)
                        or any(not isinstance(feature, str) for feature in required)):
                    raise ValueError(f"cargo metadata package {package['name']} has malformed required-features")
            if any(not isinstance(feature, str) or not isinstance(values, list)
                   or any(not isinstance(value, str) for value in values)
                   for feature, values in package['features'].items()):
                raise ValueError(f"cargo metadata package {package['name']} has malformed features")
        ids = [package['id'] for package in packages]
        names = [package['name'] for package in packages]
        if len(ids) != len(set(ids)):
            raise ValueError('cargo metadata has duplicate package IDs')
        if len(names) != len(set(names)):
            raise ValueError('cargo metadata has duplicate package names')

    def ownership_errors(self, paths, base=None):
        """Report surviving Rust/manifests that Cargo did not assign to us.

        A deleted path is allowed when it existed in the base tree.  This
        keeps package removal and ordinary file deletion valid while making a
        workspace omission or excluded crate a readiness error.
        """
        errors = []
        base_roots = _git_package_roots(self.repo, base) if base else set()
        current_roots = _current_package_roots(self.repo)
        for path in sorted(set(paths)):
            # Only paths that could be members of this repository's Cargo
            # workspace participate.  Rust snippets in scripts/templates are
            # fixtures, not omitted crates.
            if path == 'Cargo.toml' or not path.startswith('crates/'):
                continue
            if not (path.endswith('.rs') or Path(path).name == 'Cargo.toml'):
                continue
            if self.owner(path) is not None:
                continue
            current = self.repo / path
            if current.is_file():
                errors.append(f'{path}: surviving Rust/manifests has no Cargo workspace owner')
                continue
            if any(path == root or path.startswith(root + '/') for root in current_roots):
                errors.append(f'{path}: deleted Rust path remains under an unowned package')
                continue
            if base and any(path == root or path.startswith(root + '/') for root in base_roots):
                continue
            errors.append(f'{path}: Rust path has no current or base Cargo workspace owner')
        return errors

    def owner(self, path):
        owners = [name for name, root in self.roots.items()
                  if path == root or path.startswith(root + '/')]
        return max(owners, key=lambda n: len(self.roots[n])) if owners else None

    def dependencies(self, names):
        found, pending = set(), list(names)
        while pending:
            name = pending.pop()
            if name in found:
                continue
            package = self.packages[name]
            found.add(name)
            for dep in package['dependencies']:
                if dep.get('path'):
                    child = dep['name']
                    if child not in self.packages:
                        raise ValueError(f'{name}: unowned local dependency {child}')
                    pending.append(child)
        return sorted(found)

    def reverse_dependencies(self, names):
        names = set(names)
        return sorted(name for name, p in self.packages.items() if name not in names
                      and any(d['name'] in names for d in p['dependencies']))

    def feature_packages(self, feature):
        return sorted(name for name, p in self.packages.items() if feature in p['features'])

    def feature_matrix(self):
        return sorted((name, feature) for name, p in self.packages.items()
                      for feature in p['features'] if feature != 'default')

    def targets(self, name, kind=None):
        return [t for t in self.packages[name]['targets'] if kind is None or kind in t['kind']]

    def binary_owner(self, target):
        owners = [n for n in self.packages if any(t['name'] == target for t in self.targets(n, 'test'))]
        if len(owners) != 1:
            raise ValueError(f'target {target}: expected one owner, found {owners}')
        return owners[0]

    def nextest_gpu_filter(self):
        from gate_policy import NEXTTEST_GPU_FILTER, GPU_DEFAULT_CPU_ONLY
        grouped = set(re.findall(r'package\(([^)]+)\)', NEXTTEST_GPU_FILTER))
        grouped.update(identity.split('::')[0] for identity in
                       re.findall(r'binary_id\(([^)]+)\)', NEXTTEST_GPU_FILTER))
        unknown = set(self.feature_packages('gpu-proofs')) - grouped - GPU_DEFAULT_CPU_ONLY.keys()
        if unknown:
            raise ValueError(f'GPU default-test ownership unresolved: {sorted(unknown)}; add semantic grouping or a reviewed CPU-only reason')
        for package, reason in GPU_DEFAULT_CPU_ONLY.items():
            if not reason or package not in self.packages:
                raise ValueError(f'stale or unexplained default-test ownership: {package}')
        for package in re.findall(r'package\(([^)]+)\)', NEXTTEST_GPU_FILTER):
            if package not in self.packages:
                raise ValueError(f'GPU grouping has stale package {package}')
        for identity in re.findall(r'binary_id\(([^)]+)\)', NEXTTEST_GPU_FILTER):
            package, _, target = identity.partition('::')
            if package not in self.packages:
                raise ValueError(f'GPU grouping has stale owner {package}')
            if target:
                kind, _, name = target.partition('/')
                if not name:
                    kind, name = 'test', kind
                if not any(t['name'] == name for t in self.targets(package, kind)):
                    raise ValueError(f'GPU grouping has missing target {identity}')
        return NEXTTEST_GPU_FILTER

    def validate_nextest(self):
        import tomllib
        config = self.repo / '.config/nextest.toml'
        data = tomllib.loads(config.read_text())
        expected = self.nextest_gpu_filter().strip()
        for profile in ('default', 'ci'):
            rows = [r['filter'].strip() for r in data['profile'][profile]['overrides']
                    if r.get('test-group') == 'gpu']
            if rows != [expected]:
                raise ValueError(f'{config}: {profile} GPU grouping differs from semantic ownership table')


def _git_package_roots(repo, revision):
    """Return package roots present in a base tree, for deletion classification."""
    result = subprocess.run(['git', '-C', str(repo), 'ls-tree', '-r', '--name-only', revision],
                            capture_output=True, text=True)
    if result.returncode:
        return set()
    return {path[:-len('/Cargo.toml')] for path in result.stdout.splitlines()
            if path.startswith('crates/') and path.endswith('/Cargo.toml')}


def _current_package_roots(repo):
    repo = Path(repo)
    return {path.parent.relative_to(repo).as_posix()
            for path in repo.glob('crates/**/Cargo.toml') if path.is_file()}
