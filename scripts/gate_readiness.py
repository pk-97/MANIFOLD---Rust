#!/usr/bin/env python3
"""Collect cheap ownership/reference failures before builds.

Compiled test-inventory validation necessarily happens after compilation.
"""
import re
import json
import tomllib
from pathlib import Path
import subprocess

import cpu_scope
import gpu_scope
from gate_workspace import Workspace
from gate_policy import is_inert_plan_path


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


def dependency_bans_cover_workspace(repo, workspace):
    """Check the checked-in physics boundary against Cargo metadata.

    Cargo-deny remains authoritative for resolved dependency paths. This cheap
    check only ensures every current workspace crate is represented and that
    the protected lower layer cannot be admitted as a host wrapper.
    """
    path = Path(repo) / 'deny.toml'
    try:
        config = tomllib.loads(path.read_text())
        entries = config['bans']['deny']
    except (OSError, UnicodeError, tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        return [f'{path}: invalid bans config: {error}']
    if not isinstance(entries, list):
        return [f'{path}: bans.deny must be a list']

    members = set(workspace.packages)
    manifold = {name for name in members if name.startswith('manifold-')}
    foundation = 'manifold-foundation'
    required = manifold - {foundation}
    rows = {}
    problems = []
    for row in entries:
        if not isinstance(row, dict) or not isinstance(row.get('name'), str):
            continue
        name = row['name']
        if name.startswith('manifold-'):
            rows.setdefault(name, []).append(row)
            if name not in members:
                problems.append(f'{path}: stale MANIFOLD ban target {name}')
    for name in sorted(required):
        matches = rows.get(name, [])
        if not matches:
            problems.append(f'{path}: missing MANIFOLD ban entry for {name}')
            continue
        if len(matches) != 1:
            problems.append(f'{path}: expected exactly one MANIFOLD ban entry for {name}')
        wrappers = matches[0].get('wrappers')
        if not isinstance(wrappers, list) or not wrappers or any(not isinstance(w, str) or not w for w in wrappers):
            problems.append(f'{path}: {name} ban must have nonempty string wrappers')
            continue
        unknown = sorted({w for w in wrappers if w.startswith('manifold-') and w not in members})
        if unknown:
            problems.append(f'{path}: {name} has stale MANIFOLD wrappers: {", ".join(unknown)}')
        if 'manifold-ui' in wrappers:
            problems.append(f'{path}: manifold-ui cannot wrap non-foundation crate {name}')
        protected = {'manifold-physics', 'manifold-fluids', 'manifold-gpu'} & set(wrappers)
        allowed = {'manifold-fluids'} if name == 'manifold-physics' else set()
        for wrapper in sorted(protected - allowed):
            problems.append(f'{path}: protected crate {wrapper} cannot wrap non-foundation crate {name}')

    protected_dependencies = {
        'manifold-physics': {'manifold-foundation'},
        'manifold-fluids': {'manifold-foundation', 'manifold-physics'},
        'manifold-gpu': {'manifold-foundation'},
    }
    for package, allowed in protected_dependencies.items():
        if package not in members:
            continue
        dependencies = workspace.packages[package].get('dependencies', [])
        local = {dependency.get('name') for dependency in dependencies
                 if isinstance(dependency, dict) and dependency.get('path')}
        unexpected = sorted(local - allowed)
        if unexpected:
            problems.append(f'{package}: protected dependencies must stay in {sorted(allowed)}; found {unexpected}')
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


ANALYZER_ROOTS = tuple(
    f'plugins/manifold-analyzer-{name}' for name in ('dsp', 'gui', 'plugin'))
ANALYZER_MANIFESTS = {'plugins/Cargo.toml', 'plugins/Cargo.lock'}


def analyzer_paths(paths):
    """Return paths owned by the nested analyzer workspace."""
    return sorted({path for path in paths
                   if path in ANALYZER_MANIFESTS
                   or any(path == root or path.startswith(root + '/')
                          for root in ANALYZER_ROOTS)})


def analyzer_tooling(repo, paths):
    """Select focused checks for the analyzer's separate Cargo workspace."""
    if not analyzer_paths(paths):
        return []
    manifest = str(Path(repo) / 'plugins/Cargo.toml')
    checks = []
    checks.append({'name': 'analyzer-check/manifold-analyzer-plugin',
                   'argv': ['cargo', 'check', '--manifest-path', manifest,
                            '-p', 'manifold-analyzer-plugin'],
                   'cwd': str(repo), 'timeout': 600})
    for action in ('check', 'clippy'):
        for package, features in (
                ('manifold-analyzer-dsp', []),
                ('manifold-analyzer-gui', ['--features', 'gpu-proofs'])):
            argv = ['cargo', action, '--manifest-path', manifest,
                    '-p', package, '--tests', *features]
            if action == 'clippy':
                argv += ['--', '-D', 'warnings']
            checks.append({'name': f'analyzer-{action}/{package}',
                           'argv': argv, 'cwd': str(repo), 'timeout': 600})
    focused = (
        ('dsp-reference', 'manifold-analyzer-dsp', 'reference::tests::'),
        ('dsp-median', 'manifold-analyzer-dsp', 'median::tests::'),
        ('gui-precision', 'manifold-analyzer-gui', 'precision_tests::'),
        ('gui-spectrum-worker', 'manifold-analyzer-gui', 'spectrum_worker::'),
    )
    for label, package, test_filter in focused:
        features = ['--features', 'gpu-proofs'] if package.endswith('-gui') else []
        checks.append({
            'name': f'analyzer-test/{label}',
            'argv': ['cargo', 'test', '--manifest-path', manifest,
                     '-p', package, '--release', *features, test_filter],
            'cwd': str(repo), 'timeout': 600,
        })
    # The nested workspace uses Cargo patches, so run measured proofs with a
    # bounded watchdog rather than reusing a root-workspace cache receipt.
    proof = {
        'name': 'analyzer-gpu-proof',
        'argv': ['python3', 'scripts/gpu_proofs_gate.py',
                 '--manifest-path', manifest, '--package',
                 'manifold-analyzer-gui', '--filter',
                 'spectrum_gpu::spectrogram_gpu_tests::', '--budget', '120',
                 '--hang-allowance', '120'],
        'cwd': str(repo), 'timeout': 600, 'phase': 'gpu',
    }
    checks.append(dict(proof, name='analyzer-gpu-proof-build',
                       argv=[*proof['argv'], '--build-only'], phase='build'))
    checks.append(proof)
    return checks


def selected_tooling(repo, paths):
    from codex_checks import tooling_checks
    return tooling_checks(repo, paths) + analyzer_tooling(repo, paths)


def plan(repo, paths, base=None):
    paths = [path for path in paths if not is_inert_plan_path(path)]
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
        for problem in attempt('dependency-bans', lambda: dependency_bans_cover_workspace(repo, workspace)) or []:
            result['errors'].append(('dependency-bans', problem))
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
        nested_paths = set(analyzer_paths(paths))
        gpu_paths = [path for path in paths if path not in nested_paths]
        result['gpu'] = attempt('gpu-ownership', lambda: gpu_scope.plan_for_paths(
            gpu_paths, repo, base=base or 'HEAD', workspace=workspace, cpu_plan=result['cpu']))
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
