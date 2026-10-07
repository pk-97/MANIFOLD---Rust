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
        members = set(metadata['workspace_members'])
        self.packages = {p['name']: p for p in metadata['packages'] if p['id'] in members}
        if not self.packages:
            raise ValueError('cargo metadata returned no workspace packages')
        self.roots = {name: Path(p['manifest_path']).resolve().parent.relative_to(self.repo).as_posix()
                      for name, p in self.packages.items()}

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
