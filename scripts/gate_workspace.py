#!/usr/bin/env python3
"""Cargo's package, target and feature inventory. Resolution errors are red."""
import json
import os
from pathlib import Path
import subprocess
import re


def code_mask(text):
    """Keep offsets, masking nested comments and Rust string/character literals."""
    chars = list(text)
    token = re.compile(r'''//|/\*|(?:br|cr|r)(#*)"|(?:b|c)?"|(?:b)?'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^'\\\n])' '''.rstrip())
    pos = 0
    while True:
        m = token.search(text, pos)
        if not m: break
        start = m.start()
        if m[0] == '//':
            end = text.find('\n', m.end())
            if end < 0: end = len(text)
        elif m[0] == '/*':
            depth, end = 1, m.end()
            while depth and end < len(text):
                if text.startswith('/*', end): depth += 1; end += 2
                elif text.startswith('*/', end): depth -= 1; end += 2
                else: end += 1
        elif m[1] is not None:
            closer = '"' + m[1]
            end = text.find(closer, m.end())
            end = len(text) if end < 0 else end + len(closer)
        elif m[0].endswith('"'):
            end = m.end()
            while end < len(text):
                if text[end] == '\\': end += 2
                elif text[end] == '"': end += 1; break
                else: end += 1
        else: end = m.end()
        chars[start:end] = ['\n' if c == '\n' else ' ' for c in text[start:end]]
        pos = end
    return ''.join(chars)


def _testkit_calls(text):
    """Return testkit_visible! calls and the selected production arm span."""
    masked = code_mask(text)
    call = re.compile(
        r'(?<![\w$])(?:\$crate|[A-Za-z_]\w*)(?:\s*::\s*[A-Za-z_]\w*)*'
        r'\s*::\s*testkit_visible\s*!\s*(?P<open>[({[])|'
        r'(?<![\w$])testkit_visible\s*!\s*(?P<bare_open>[({[])')
    calls = []

    def matching(opening):
        pairs = {'(': ')', '[': ']', '{': '}'}
        closing = pairs[masked[opening]]
        depth = 0
        for index in range(opening, len(masked)):
            if masked[index] == masked[opening]:
                depth += 1
            elif masked[index] == closing:
                depth -= 1
                if depth == 0:
                    return index
        raise ValueError('unclosed testkit_visible! invocation')

    for match in call.finditer(masked):
        opening = match.start('open') if match.group('open') else match.start('bare_open')
        closing = matching(opening)
        body_start, body_end = opening + 1, closing
        def skip_space(index):
            while index < body_end and masked[index].isspace():
                index += 1
            return index

        def arm(name, index):
            found = re.match(rf'{name}\b\s*\{{', masked[index:body_end])
            if not found:
                return None
            arm_open = index + found.group(0).rfind('{')
            arm_close = matching(arm_open)
            return arm_close + 1, (arm_open + 1, arm_close)

        index = skip_space(body_start)
        first = arm('testkit', index)
        if first is None:
            selected, dual = (body_start, body_end), False
        else:
            index, _testkit = first
            second = arm('production', skip_space(index))
            if second is None or skip_space(second[0]) != body_end:
                raise ValueError('malformed testkit_visible! dual arm')
            selected, dual = second[1], True
        calls.append((match.start(), closing + 1, *selected, dual))
    return calls


def production_text(text):
    """Replace testkit_visible! calls with their production item, preserving offsets."""
    result = list(text)
    calls = _testkit_calls(text)
    active = []
    for call in calls:
        start, end, selected_start, selected_end, _dual = call
        discarded = any(other_start <= start and end <= other_end
                        and not (other_selected_start <= start and end <= other_selected_end)
                        for other_start, other_end, other_selected_start, other_selected_end, _other_dual in calls
                        if (other_start, other_end) != (start, end))
        if not discarded:
            active.append(call)
    for start, end, selected_start, selected_end, _dual in sorted(active):
        selected = text[selected_start:selected_end]
        result[start:end] = ['\n' if char == '\n' else ' ' for char in text[start:end]]
        result[selected_start:selected_end] = selected
    return ''.join(result)


IDENT = r'(?:r#)?[A-Za-z_][A-Za-z_0-9]*'
VIS = r'(?:pub(?:\((?:crate|super|in ' + IDENT + r'(?:::' + IDENT + r')*)\))? +)?'


def module_items(text):
    """Locate module-level items; expand testkit_visible! but keep other macros opaque."""
    masked = code_mask(production_text(text))
    tokens = list(re.finditer(IDENT + r'|[^\s]', masked))
    result = []
    def scan(pos, scope):
        start = None; header = None
        while pos < len(tokens):
            token = tokens[pos]; value = token[0]
            if value == '}': return pos + 1
            if value == '#' and pos + 2 < len(tokens) and tokens[pos+1][0] == '!' and tokens[pos+2][0] == '[':
                pos = skip(pos+2); start = header = None; continue
            if start is None: start = token.start()
            if value == '#' and pos + 1 < len(tokens) and tokens[pos+1][0] == '[':
                pos = skip(pos+1); continue
            if header is None: header = token.start()
            if value == '{':
                head = masked[header:token.start()].strip()
                mod = re.fullmatch(VIS + r'mod (' + IDENT + ')', head)
                end = scan(pos+1, scope+(mod[1],)) if mod else skip(pos)
                result.append((start, tokens[end-1].end(), header, scope))
                pos = end; start = header = None; continue
            if value in ('(', '['): pos = skip(pos); continue
            if value == ';':
                result.append((start, token.end(), header, scope))
                start = header = None
            pos += 1
        return pos
    def skip(pos):
        stack = []
        pairs = {'(': ')', '[': ']', '{': '}'}
        while pos < len(tokens):
            value = tokens[pos][0]
            if value in pairs: stack.append(pairs[value])
            elif value in (')', ']', '}'):
                if not stack or value != stack.pop(): raise ValueError('unbalanced Rust delimiters')
                if not stack: return pos+1
            pos += 1
        raise ValueError('unclosed Rust delimiter')
    scan(0, ())
    return result


def module_mounts(root):
    """Resolve source files to their module paths in one Rust target."""
    found = {}

    def walk(source, prefix, ancestors):
        source = source.resolve()
        if source in ancestors or not source.is_file():
            return
        found.setdefault(source, set()).add(prefix)
        text = source.read_text()
        for start, end, head, scope in module_items(text):
            declaration = re.match(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", text[head:end])
            if not declaration:
                continue
            name = declaration[1]
            attrs = re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', text[start:head])
            if attrs:
                child = source.parent.joinpath(*scope, attrs[-1])
            else:
                base = source.parent if not ancestors or source.stem in ("lib", "mod", "main") else source.with_suffix("")
                child = base.joinpath(*scope, name + ".rs")
                if not child.is_file():
                    child = base.joinpath(*scope, name, "mod.rs")
            walk(child, prefix + scope + (name,), ancestors | {source})

    walk(Path(root), (), set())
    return found


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
