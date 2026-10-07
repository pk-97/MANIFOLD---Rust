#!/usr/bin/env python3
"""Collect cheap ownership/reference failures before builds.

Compiled test-inventory validation necessarily happens after compilation.
"""
import re
import json
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


def executable_problems(repo, paths, checks=None):
    """Check selected tooling entrypoints before any package build starts."""
    problems = []
    # Repository CLI scripts and selected checks are direct entrypoints.
    # Interpreter-loaded hooks/modules do not require executable mode.
    candidates = {path for path in paths if path.startswith('scripts/')}
    if checks is None:
        checks = selected_tooling(repo, paths)
    required = set()
    for check in checks:
        for name in [check.get('name', ''), *check.get('argv', [])]:
            if not isinstance(name, str) or not name.endswith(('.py', '.sh')):
                continue
            candidate = Path(name)
            if not candidate.is_absolute():
                candidate = Path(repo) / candidate
            relative = candidate.resolve().relative_to(Path(repo).resolve()).as_posix()
            if relative.startswith('scripts/') or check.get('argv', [])[:1] == [name]:
                required.add(relative)
    for relative in sorted(candidates | required):
        path = Path(repo) / relative
        if path.suffix not in {'.py', '.sh'}:
            continue
        if not path.is_file():
            if relative in required:
                problems.append(f'{relative}: missing executable entrypoint')
            continue
        try:
            source = path.read_text()
            first = next(iter(source.splitlines()), '')
            executable = bool(path.stat().st_mode & 0o111)
        except (OSError, UnicodeError) as error:
            problems.append(f'{relative}: cannot inspect entrypoint: {error}')
            continue
        if relative in required and not first.startswith('#!'):
            problems.append(f'{relative}: executable entrypoint has no shebang')
        entrypoint = (relative in required or path.suffix == '.sh'
                      or re.search(r'''if\s+__name__\s*==\s*['"]__main__['"]''', source))
        if entrypoint and first.startswith('#!') and not executable:
            problems.append(f'{relative}: shebang entrypoint is not executable')
    return problems


def flow_problems(repo, paths):
    """Validate the static UI-flow manifest and its referenced scripts."""
    manifest_path = Path(repo) / 'scripts/ui-flows/manifest.json'
    try:
        manifest = json.loads(manifest_path.read_text())
    except (OSError, ValueError, TypeError) as error:
        return [f'{manifest_path}: invalid UI-flow manifest: {error}']
    if not isinstance(manifest, dict):
        return [f'{manifest_path}: manifest must be an object']
    problems = []
    flows = manifest.get('flows')
    xfail = manifest.get('expected_fail', {})
    unresolved = manifest.get('unresolved', {})
    triggers = manifest.get('path_triggers', {})
    if not isinstance(flows, dict):
        problems.append(f'{manifest_path}: flows must be an object')
        flows = {}
    if not isinstance(xfail, dict):
        problems.append(f'{manifest_path}: expected_fail must be an object')
        xfail = {}
    if not isinstance(unresolved, dict):
        problems.append(f'{manifest_path}: unresolved must be an object')
        unresolved = {}
    if not isinstance(triggers, dict) or any(
            not isinstance(prefix, str) or not isinstance(names, list)
            or any(not isinstance(name, str) for name in names)
            for prefix, names in triggers.items()):
        problems.append(f'{manifest_path}: path_triggers must map paths to string lists')
    accounted = set(flows) | set(xfail) | set(unresolved)
    flow_dir = manifest_path.parent
    on_disk = {p.stem for p in flow_dir.glob('*.json') if p.name != 'manifest.json'}
    for name in sorted(accounted - on_disk):
        problems.append(f'{manifest_path}: stale flow entry {name}')
    for name in sorted(on_disk - accounted):
        problems.append(f'{manifest_path}: unaccounted flow file {name}.json')
    for name, scene in flows.items():
        if not isinstance(name, str) or not isinstance(scene, str) or not scene:
            problems.append(f'{manifest_path}: flow {name!r} must map to a scene')
    for name, value in xfail.items():
        if (not isinstance(value, dict) or not isinstance(value.get('scene'), str)
                or not value.get('scene') or not isinstance(value.get('bug'), str)
                or not isinstance(value.get('reason'), str)):
            problems.append(f'{manifest_path}: expected_fail {name!r} needs scene, bug and reason')
    for name, value in unresolved.items():
        if not isinstance(value, str) or not value:
            problems.append(f'{manifest_path}: unresolved {name!r} needs a reason')
    overlaps = (set(flows) & set(xfail)) | (set(flows) & set(unresolved)) | (set(xfail) & set(unresolved))
    for name in sorted(overlaps, key=str):
        problems.append(f'{manifest_path}: flow {name!r} appears in multiple status sections')
    if isinstance(triggers, dict):
        for prefix, names in triggers.items():
            if not isinstance(names, list):
                continue
            for name in names:
                # The runner uses substring filters, not exact flow names.
                if isinstance(name, str) and not any(name in flow for flow in accounted):
                    problems.append(f'{manifest_path}: path trigger {prefix!r} names unknown flow {name!r}')
    for name in sorted(on_disk & accounted):
        flow = flow_dir / f'{name}.json'
        try:
            actions = json.loads(flow.read_text())
            if not isinstance(actions, list):
                problems.append(f'{flow}: flow script must be a JSON array')
        except (OSError, ValueError, TypeError) as error:
            problems.append(f'{flow}: invalid flow script: {error}')
    try:
        import run_ui_flows
        run_ui_flows.filters_for_paths(paths, manifest)
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        problems.append(f'{manifest_path}: cannot map touched paths: {error}')
    return problems


def flow_filters(repo, paths):
    """Return the pre-build UI-flow selection after manifest validation."""
    manifest_path = Path(repo) / 'scripts/ui-flows/manifest.json'
    manifest = json.loads(manifest_path.read_text())
    import run_ui_flows
    return run_ui_flows.filters_for_paths(paths, manifest)[0]


def selected_tooling(repo, paths):
    from codex_checks import tooling_checks
    return tooling_checks(repo, paths)


def plan(repo, paths, base=None):
    result = {'packages': [], 'dependents': [], 'cpu': None, 'gpu': None,
              'workspace': None, 'errors': [],
              'flows': [], 'tooling': [],
              'notes': ['compiled test inventories are validated after compilation; '
                        'readiness checks static Cargo ownership and references']}

    def attempt(label, action):
        try:
            return action()
        except (OSError, ValueError, KeyError, TypeError, RuntimeError,
                AttributeError, subprocess.SubprocessError) as error:
            result['errors'].append((label, str(error)))

    tooling = attempt('tooling-selection', lambda: selected_tooling(repo, paths))
    result['tooling'] = tooling or []
    for label, action in (('entrypoints', lambda: executable_problems(repo, paths, result['tooling'])),
                          ('ui-flows', lambda: flow_problems(repo, paths))):
        for problem in attempt(label, action) or []:
            result['errors'].append((label, problem))
    if not any(label == 'ui-flows' for label, _ in result['errors']):
        result['flows'] = attempt('ui-flow-selection', lambda: flow_filters(repo, paths)) or []

    workspace = attempt('metadata', lambda: Workspace(repo))
    result['workspace'] = workspace
    if workspace:
        attempt('nextest-grouping', workspace.validate_nextest)
        result['packages'] = attempt('package-ownership',
                                     lambda: sorted({workspace.owner(p) for p in paths} - {None})) or []
        result['dependents'] = attempt('reverse-dependencies',
                                       lambda: workspace.reverse_dependencies(result['packages'])) or []
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
        result['gpu'] = attempt('gpu-ownership', lambda: gpu_scope.plan_for_paths(
            paths, repo, base=base or 'HEAD', workspace=workspace, cpu_plan=result['cpu']))
        if result['gpu'] and result['gpu'].unmapped:
            result['errors'].append(('gpu-ownership', gpu_scope.unmapped_message(result['gpu'])))
        references = attempt('references', lambda: reference_problems(repo, workspace, result['packages']))
        result['errors'].extend(('references', p) for p in references or [])
    from codex_regressions import inventory
    attempt('regression-inventory', lambda: inventory(repo))
    return result


def describe(result):
    return {'packages': result['packages'], 'dependents': result['dependents'],
            'flows': result.get('flows', []),
            'tooling': result.get('tooling', []),
            'cpu': result['cpu'].selections() if result['cpu'] else {},
            'whole_default_suites': sorted(result['cpu'].whole) if result['cpu'] else [],
            'gpu': result['gpu'].runs() if result['gpu'] and not result['gpu'].unmapped else [],
            'errors': result['errors'],
            'notes': result.get('notes', [])}
