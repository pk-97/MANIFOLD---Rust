#!/usr/bin/env python3
"""Read-only landing planning; collect independent ownership/reference failures."""
import re
from pathlib import Path
import subprocess

import cpu_scope
import gpu_scope
from gate_workspace import Workspace


def reference_problems(repo, workspace, packages):
    problems = []
    for name in packages:
        root = Path(repo) / workspace.roots[name]
        for target in workspace.targets(name):
            if not Path(target['src_path']).is_file():
                problems.append(f'{name}: missing target source {target["src_path"]}')
        for source in root.rglob('*.rs'):
            if 'target' in source.parts:
                continue
            text = re.sub(r'^\s*//[^\n]*', '', source.read_text(), flags=re.M)
            for match in re.finditer(r'include_(?:str|bytes)!\s*\(\s*"([^"\n]+)"\s*\)', text):
                target = source.parent / match[1]
                if not target.is_file():
                    problems.append(f'{source.relative_to(repo)}: missing include {match[1]}')
            for match in re.finditer(
                    r'include_(?:str|bytes)!\s*\(\s*concat!\s*\(\s*env!\("CARGO_MANIFEST_DIR"\),\s*"([^"\n]+)"\s*\)\s*\)', text):
                if not (root / match[1].lstrip('/')).is_file():
                    problems.append(f'{source.relative_to(repo)}: missing manifest include {match[1]}')
    return problems


def plan(repo, paths, base=None):
    result = {'packages': [], 'dependents': [], 'cpu': None, 'gpu': None,
              'workspace': None, 'errors': []}

    def attempt(label, action):
        try:
            return action()
        except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
            result['errors'].append((label, str(error)))

    workspace = attempt('metadata', lambda: Workspace(repo))
    result['workspace'] = workspace
    if workspace:
        attempt('nextest-grouping', workspace.validate_nextest)
        result['packages'] = sorted({workspace.owner(p) for p in paths} - {None})
        result['dependents'] = workspace.reverse_dependencies(result['packages'])
        result['cpu'] = attempt('cpu-ownership', lambda: cpu_scope.plan_for_paths(paths, repo, workspace, base))
        if result['cpu']:
            for package in result['cpu'].packages:
                if package not in workspace.packages:
                    result['errors'].append(('cpu-ownership', f'unknown mapped package {package}'))
            for expression in result['cpu'].filters:
                package = re.search(r'package\(=([^)]*)\)', expression)[1]
                target = re.search(r'binary\(=([^)]*)\)', expression)
                if target and (package not in workspace.packages or not any(
                        t['name'] == target[1] for t in workspace.targets(package))):
                    result['errors'].append(('cpu-ownership', f'missing target: {expression}'))
        result['gpu'] = attempt('gpu-ownership', lambda: gpu_scope.plan_for_paths(paths, repo, base=base or 'HEAD', workspace=workspace))
        if result['gpu'] and result['cpu']:
            result['gpu'].required_binaries.update(result['cpu'].gpu_binaries)
        if result['gpu'] and result['gpu'].unmapped:
            result['errors'].append(('gpu-ownership', gpu_scope.unmapped_message(result['gpu'])))
        references = attempt('references', lambda: reference_problems(repo, workspace, result['packages']))
        result['errors'].extend(('references', p) for p in references or [])
    from codex_regressions import inventory
    attempt('regression-inventory', lambda: inventory(repo))
    return result


def describe(result):
    return {'packages': result['packages'], 'dependents': result['dependents'],
            'cpu': result['cpu'].selections() if result['cpu'] else {},
            'whole_default_suites': sorted(result['cpu'].whole) if result['cpu'] else [],
            'gpu': result['gpu'].runs() if result['gpu'] and not result['gpu'].unmapped else [],
            'errors': result['errors']}
