#!/usr/bin/env python3
"""Shared content-addressed successful checks; never a substitute for gate scope.

Records live in the Git common directory, outside every checkout. Fingerprints
use Git blob IDs and modes (the leaves of the selected Git trees), overlaying
working-tree edits so a standalone pass survives committing the same content.
Unknown commands/dependencies and unreadable inputs run without reuse.
"""
import hashlib
import json
import os
import platform
import subprocess
import sys
import tempfile
import tomllib
from datetime import datetime, timezone
from pathlib import Path

SCHEMA = 1


def git(repo, *args):
    result = subprocess.run(['git', '-C', str(repo), *args], capture_output=True,
                            timeout=30)
    if result.returncode:
        raise ValueError(result.stderr.decode(errors='replace').strip())
    return result.stdout.decode().strip('\n')


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def dependency_paths(repo, packages):
    """All local normal/build/dev/optional/target dependencies, transitively.

    Read manifests rather than resolving registry packages or building. Cargo.lock
    pins external packages. External path dependencies cannot be fingerprinted.
    """
    root = tomllib.loads((repo / 'Cargo.toml').read_text())
    if root.get('patch') or root.get('replace'):
        raise ValueError('Cargo patch/replace requires a dependency-scope audit')
    workspace = root.get('workspace', {})
    manifests = {}
    for pattern in workspace.get('members', []):
        for directory in repo.glob(pattern):
            data = tomllib.loads((directory / 'Cargo.toml').read_text())
            manifests[data['package']['name']] = (directory, data)
    found, pending = set(), list(packages)
    while pending:
        name = pending.pop()
        if name in found:
            continue
        if name not in manifests:
            raise ValueError(f'unknown local package {name}')
        found.add(name)
        directory, data = manifests[name]
        tables = [data, *data.get('target', {}).values()]
        for table in tables:
            for kind in ('dependencies', 'dev-dependencies', 'build-dependencies'):
                for alias, dep in table.get(kind, {}).items():
                    if not isinstance(dep, dict):
                        continue
                    owner = directory
                    if dep.get('workspace'):
                        dep = workspace.get('dependencies', {})[alias]
                        owner = repo
                    if isinstance(dep, dict) and 'path' in dep:
                        path = (owner / dep['path']).resolve()
                        if not path.is_relative_to(repo):
                            raise ValueError(f'external path dependency: {path}')
                        child = tomllib.loads((path / 'Cargo.toml').read_text())
                        child_name = child['package']['name']
                        manifests[child_name] = (path, child)
                        pending.append(child_name)
    return sorted(str(manifests[n][0].relative_to(repo)) for n in found)


def host_inputs(repo, cargo=False):
    # Keep unknown environment variables: test switches must never disappear
    # from the key. Only process identity, lock tokens and build-cache placement
    # are ignored. Values are hashed, never written to the record.
    ignored = {'PWD', 'OLDPWD', 'SHLVL', '_', 'CARGO_TARGET_DIR',
               'CARGO_BUILD_TARGET_DIR', 'CARGO_INCREMENTAL', 'CARGO_BUILD_JOBS',
               'MANIFOLD_GPU_LOCK_HOLDER'}
    # Agent session/IPC identities do not configure Rust or libtest. Keeping
    # them would prevent sharing between a worker and its landing lead.
    # Python tooling can inspect those variables, so it retains them.
    agent_prefixes = ('CLAUDE_', 'CODEX_', 'ANTHROPIC_', 'OPENAI_') if cargo else ()
    if cargo:
        ignored.update({'CLAUDECODE', 'CLAUDE_PID', 'AI_AGENT', 'BAGGAGE'})
    env = {k: v for k, v in os.environ.items()
           if k not in ignored and not k.startswith(agent_prefixes)}
    facts = {'environment': digest(env), 'host': platform.node(),
             'platform': platform.platform(), 'python': sys.version}
    if cargo:
        commands = [['rustc', '-vV'], ['cargo', '-V'],
                        ['cargo', 'clippy', '-V'], ['cargo', 'nextest', '--version'],
                        ['cargo', 'deny', '--version']]
        if sys.platform == 'darwin':
            commands += [['clang', '--version'], ['xcrun', '--show-sdk-version']]
        for command in commands:
            out = subprocess.run(command, cwd=repo, capture_output=True, timeout=15)
            if out.returncode:
                raise ValueError(f'cannot identify tool: {command}')
            facts[' '.join(command)] = out.stdout.decode()
        # Cargo reads config in ancestors and CARGO_HOME, outside Git.
        homes = [*repo.parents, Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))]
        configs = {p for home in homes for p in
                   (home / '.cargo/config', home / '.cargo/config.toml',
                    home / 'config.toml', home / 'config') if p.is_file()}
        configs.update(p for p in (repo / '.cargo/config', repo / '.cargo/config.toml') if p.is_file())
        for path in configs:
            config = tomllib.loads(path.read_text())
            if any(config.get(key) for key in ('paths', 'patch', 'source')):
                raise ValueError(f'Cargo source override needs an input-scope audit: {path}')
        if any(k.startswith(('CARGO_SOURCE_', 'CARGO_PATCH_')) for k in env):
            raise ValueError('environment overrides Cargo sources outside the manifest graph')
        facts['cargo-config'] = [(str(p), hashlib.sha256(p.read_bytes()).hexdigest())
                                 for p in sorted(configs) if not p.is_relative_to(repo)]
    return facts


def selected_entries(repo, prefixes):
    def selected(path):
        return any(path == p or path.startswith(p.rstrip('/') + '/') for p in prefixes)
    entries = {}
    for item in git(repo, 'ls-tree', '-r', '-z', 'HEAD').split('\0'):
        if not item:
            continue
        meta, path = item.split('\t', 1)
        if selected(path):
            mode, kind, sha = meta.split()
            if kind != 'blob' or mode == '120000':
                raise ValueError(f'unsupported submodule/symlink input: {path}')
            entries[path] = [mode, sha]
    dirty = git(repo, 'diff', '--name-only', '-z', 'HEAD').split('\0')
    untracked = git(repo, 'ls-files', '--others', '--exclude-standard', '-z').split('\0')
    # Fixtures ignored by Git still change test results. Limit the scan to
    # selected input roots; never walk target or another slot's checkout.
    roots = [p for p in prefixes if p.split('/')[0] in {'crates', 'tests', 'assets', 'tools'}]
    ignored = git(repo, 'ls-files', '--others', '--ignored', '--exclude-standard', '-z',
                  '--', *roots).split('\0') if roots else []
    for path in set(dirty + untracked + ignored):
        if not path or not selected(path):
            continue
        if any(p in {'target', '__pycache__', '.DS_Store'} for p in Path(path).parts):
            continue
        file = repo / path
        if file.is_symlink():
            raise ValueError(f'symlink input: {path}')
        if not file.exists():
            entries.pop(path, None)
        else:
            content = file.read_bytes()
            blob = hashlib.sha1(b'blob ' + str(len(content)).encode() + b'\0' + content).hexdigest()
            entries[path] = ['100755' if file.stat().st_mode & 0o111 else '100644', blob]
    return entries


def rust_paths(repo, packages):
    # Non-crate runtime assets, native sources, fixtures and build helpers are
    # shared inputs. CPU doc contracts add docs separately below; renderer
    # proofs read the generated node catalog, not design prose or bead history.
    return dependency_paths(repo, packages) + [
        'Cargo.toml', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml',
        '.cargo', '.config', 'deny.toml', 'clippy.toml', '.clippy.toml',
        'scripts', 'tests', 'assets', 'tools', 'native', 'shaders', 'vendor',
        'crates/manifold-foundation/assets/fonts',
        'docs/node_catalog', '.gitignore']


def command_spec(repo, label, cmd):
    packages = [cmd[i + 1] for i, a in enumerate(cmd[:-1]) if a in ('-p', '--package')]
    if cmd[:2] in (['cargo', 'clippy'], ['cargo', 'nextest']):
        if not packages:
            raise ValueError('Rust leg has no explicit packages')
        # CPU integration tests include docs_index_sync and docs_lifecycle.
        return rust_paths(repo, packages) + ['docs', '.claude/hooks'], cmd, True
    if label == 'deny':
        workspace = tomllib.loads((repo / 'Cargo.toml').read_text())['workspace']
        packages = [tomllib.loads((p / 'Cargo.toml').read_text())['package']['name']
                    for pattern in workspace['members'] for p in repo.glob(pattern)]
        manifests = [p + '/Cargo.toml' for p in dependency_paths(repo, packages)]
        return manifests + ['Cargo.toml', 'Cargo.lock', 'deny.toml', '.cargo',
                            'rust-toolchain', 'rust-toolchain.toml', 'scripts'], cmd, True
    if label == 'docs-index':
        return ['docs', 'scripts'], cmd, False
    # Diff-based checks depend on both sides, not the spelling of a moving ref.
    if label in {'design-status', 'flow-gate'}:
        normalized = list(cmd)
        if label == 'design-status':
            normalized[-2:] = [git(repo, 'rev-parse', f'{ref}^{{tree}}') for ref in cmd[-2:]]
        else:
            left, right = cmd[-1].split('...')
            base = git(repo, 'merge-base', left, right)
            normalized[-1] = [git(repo, 'rev-parse', f'{ref}^{{tree}}') for ref in (base, right)]
        return ['crates', 'docs', 'scripts', '.claude/hooks', 'tests', 'assets', 'tools',
                'Cargo.toml', 'Cargo.lock', '.cargo', '.config'], normalized, label == 'flow-gate'
    # Tooling and ratchet checks can inspect any tracked source. Whole-tree
    # scope is deliberately conservative; never guess a test's imports.
    roots = [p.split('/', 1)[0] for p in git(repo, 'ls-files', '-z').split('\0') if p]
    return sorted(set(roots)), cmd, False


class Pass:
    def __init__(self, repo, label, spec):
        self.repo, self.label, self.spec = Path(repo).resolve(), label, spec
        self.key = self.record = None
        self.reason = None
        try:
            self.key = self.fingerprint()
            common = git(self.repo, 'rev-parse', '--git-common-dir')
            self.directory = (self.repo / common).resolve() / 'gate-passes-v1'
            self.path = self.directory / (self.key + '.json')
            if self.path.exists():
                record = json.loads(self.path.read_text())
                if (isinstance(record, dict) and record.get('schema') == SCHEMA and record.get('key') == self.key
                        and record.get('pass') is True and isinstance(record.get('seconds'), (int, float))
                        and record['seconds'] >= 0 and record.get('commit') and record.get('time')):
                    self.record = record
        except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
            self.key = None
            self.reason = str(error)
            print(f'[NO REUSE] {label}: {error}', flush=True)

    def fingerprint(self):
        paths, identity, cargo = self.spec()
        paths = sorted(set(paths + ['scripts/gate_passes.py', 'scripts/landing_gate.py']))
        return digest({'schema': SCHEMA, 'identity': identity,
                       'paths': paths, 'entries': selected_entries(self.repo, paths),
                       'implementation': {
                           name: hashlib.sha256((Path(__file__).parent / name).read_bytes()).hexdigest()
                           for name in ('gate_passes.py', 'landing_gate.py', 'gpu_proofs_gate.py',
                                        'gpu_queue.py', 'gpu_scope.py', 'cpu_scope.py', 'diff_scope.py')},
                       'host': host_inputs(self.repo, cargo)})

    def reused(self):
        if self.record:
            print(f"[REUSED] {self.label} (passed at {self.record['commit']}, {self.record['time']})", flush=True)
            return True
        return False

    def save(self, code, seconds=0):
        if not self.key:
            return
        try:
            if code:
                self.path.unlink(missing_ok=True)
                return
            if self.fingerprint() != self.key:
                print(f'[NO REUSE] {self.label}: inputs changed during execution', flush=True)
                return
            self.directory.mkdir(parents=True, exist_ok=True)
            record = {'schema': SCHEMA, 'key': self.key, 'pass': True,
                      'commit': git(self.repo, 'rev-parse', 'HEAD'),
                      'time': datetime.now(timezone.utc).isoformat(), 'seconds': seconds}
            fd, temporary = tempfile.mkstemp(dir=self.directory, prefix='.pass-')
            with os.fdopen(fd, 'w') as stream:
                json.dump(record, stream)
            os.replace(temporary, self.path)
        except (OSError, ValueError, KeyError, TypeError, AttributeError, subprocess.SubprocessError) as error:
            print(f'[NO REUSE] {self.label}: cannot record pass: {error}', flush=True)


def command_pass(repo, label, cmd):
    def spec():
        paths, identity, cargo = command_spec(Path(repo).resolve(), label, cmd)
        identity = [a.replace(str(Path(repo).resolve()) + '/', '') if isinstance(a, str) else a
                    for a in identity]
        return paths, identity, cargo
    return Pass(repo, label, spec)


def proof_pass(repo, run):
    # Canonical per-invocation selection shared by the proof gate and queue.
    package = run.get('package', 'manifold-renderer')
    identity = {'kind': 'gpu-proof-run', 'package': package, 'features': ['gpu-proofs'],
                'targets': sorted(run['targets'] if run['targets'] is not None
                                  else ([] if run['lib'] else ['gpu_proofs'])), 'lib': run['lib'],
                'filters': sorted(run['filters']), 'skips': sorted(run['skips']),
                'test-threads': 1}
    return Pass(repo, 'gpu-proofs', lambda: (rust_paths(Path(repo).resolve(), [package]),
                                           identity, True))


def queued_proof(command, repo):
    """Recognize only the gate-equivalent cargo test grammar; else execute normally."""
    if command[:2] != ['cargo', 'test'] or '--no-run' in command:
        return None
    run = {'targets': [], 'lib': False, 'filters': [], 'skips': []}
    packages, features, threads = [], [], None
    args = iter(command[2:])
    try:
        for arg in args:
            if arg == '--':
                break
            if arg in ('-p', '--package'):
                packages.append(next(args))
            elif arg == '--features':
                features.extend(next(args).replace(',', ' ').split())
            elif arg == '--test':
                run['targets'].append(next(args))
            elif arg == '--lib':
                run['lib'] = True
            elif arg == '--manifest-path':
                if (Path(repo) / next(args)).resolve() != (Path(repo) / 'Cargo.toml').resolve():
                    return None
            elif not arg.startswith('-'):
                run['filters'].append(arg)
            elif arg != '--no-fail-fast':
                return None
        for arg in args:
            if arg == '--test-threads=1':
                threads = 1
            elif arg == '--test-threads':
                threads = int(next(args))
            elif arg == '--skip':
                run['skips'].append(next(args))
            elif arg.startswith('-'):
                return None
            else:
                run['filters'].append(arg)
    except (StopIteration, ValueError):
        return None
    if (len(packages) != 1 or packages[0] not in (
            'manifold-renderer', 'manifold-node-engine', 'manifold-ui-paint')
            or sorted(features) != ['gpu-proofs'] or threads != 1):
        return None
    if not (run['targets'] or run['lib']):
        return None
    run['package'] = packages[0]
    return proof_pass(repo, run)
