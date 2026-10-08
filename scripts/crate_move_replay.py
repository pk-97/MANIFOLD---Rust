#!/usr/bin/env python3
"""Deterministically replay reviewed crate moves and verify whole Git tree identity."""
import argparse
import bisect
import json
import hashlib
import shutil
import unicodedata
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import tempfile
import sys
import tomllib

CONFIG = {}
R = 'crates/manifold-nodes/'
E = 'crates/manifold-node-engine/'
MODULES = {}

def read_utf8(path):
    return path.read_bytes().decode('utf-8')

def tsv(path):
    rows = []
    for n, line in enumerate(read_utf8(Path(path)).splitlines(), 1):
        if not line.strip() or line.startswith('#'): continue
        cells = line.split('\t')
        if len(cells) != 2 or not all(cells): raise ValueError(f'{path}:{n}: expected two nonempty tab-separated columns')
        rows.append(tuple(cells))
    return rows

def safe_path(p):
    q = PurePosixPath(p)
    if not p or p != q.as_posix() or q.is_absolute() or any(x in ('..', '.git', 'target') for x in q.parts) or '\\' in p:
        raise ValueError(f'unsafe relative path: {p}')
    return p

def module(path, modules=None):
    modules = MODULES if modules is None else modules
    if path in modules: return modules[path]
    # Cargo binaries have their own crate roots, not library module paths.
    if '/src/bin/' in path: return None
    if not path.startswith('crates/') or '/src/' not in path or not path.endswith('.rs'): return None
    package, rest = path.split('/src/', 1)
    root = package.rsplit('/', 1)[1].replace('-', '_')
    parts = rest[:-3].split('/')
    if parts[-1] in ('lib', 'mod'): parts.pop()
    return '::'.join([root] + parts)

def split_use(s):
    depth = 0; start = 0; result = []
    for i, c in enumerate(s):
        if c == '{': depth += 1
        elif c == '}': depth -= 1
        elif c == ',' and depth == 0:
            result.append(s[start:i].strip()); start = i+1
    result.append(s[start:].strip())
    return [x for x in result if x]

def expand_use(s, prefix=''):
    out = []
    for part in split_use(s):
        if '{' in part:
            head, tail = part.split('{', 1)
            if not tail.endswith('}'): raise ValueError('unsupported use tree: '+s)
            out.extend(expand_use(tail[:-1], prefix+head.strip()))
        else:
            out.append(prefix+part)
    return out

USE = re.compile(r'(?m)^([ \t]*)(pub(?:\([^\n)]*\))?\s+)?use\s+([^;]+);')

def mappings(moves, rewrites, modules=None):
    result = {}
    for a, b in rewrites:
        a, b = rust_path(a), rust_path(b)
        if a in result and result[a] != b: raise ValueError('conflicting rewrite: ' + a)
        result[a] = b
    for a, b in moves.items():
        if module(a, modules) and module(b, modules):
            key, value = rust_path(module(a, modules)), rust_path(module(b, modules))
            if key in result and result[key] != value: raise ValueError('conflicting derived rewrite: ' + key)
            result[key] = value
    return result

def inline_modules(source):
    token=re.compile(r'\bmod\s+([A-Za-z_]\w*)\s*\{|[{}]')
    offsets=[0]; scopes=[()]; stack=[]; modules=[]
    for match in token.finditer(code_mask(source)):
        item=match[0]
        if match[1]: stack.append(True); modules.append(match[1])
        elif item=='{': stack.append(False)
        elif stack and stack.pop(): modules.pop()
        offsets.append(match.end()); scopes.append(tuple(modules))
    return offsets,scopes

def collect_path_modules(source,moves):
    MODULES.clear()
    sources = {str(p.relative_to(source)): read_utf8(p)
               for p in sorted((source/(R+'src')).rglob('*.rs')) if not p.is_symlink()}
    MODULES.update(path_modules(sources, moves, lambda path: (source/path).is_file()))


def path_modules(sources, moves, exists):
    """Shared module ownership discovery for disk replay and Git-blob checking."""
    modules = {}
    rows=[]
    for parent, text in sorted(sources.items()):
        masked = code_mask(text)
        offsets, scopes = inline_modules(text)
        for start, end, head, scope in module_items(text):
            declaration = re.fullmatch(VIS + r'mod (' + IDENT + r')\s*;', text[head:end])
            attrs = re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', text[start:head])
            if not declaration or not attrs:
                continue
            child=os.path.normpath(str(PurePosixPath(parent).parent/attrs[-1]))
            if exists(child):
                rows.append((parent,child,scope+(declaration[1],)))
        for m in re.finditer(r'include!\("([^"]+\.rs)"\)',text):
            if masked[m.start()] != 'i': continue
            child=os.path.normpath(str(PurePosixPath(parent).parent/m[1]))
            if exists(child):
                rows.append((parent,child,scopes[bisect.bisect_right(offsets,m.start())-1]))
    for _ in range(4):
        for parent,child,names in rows:
            if module(parent, modules): modules[child]='::'.join((module(parent, modules),)+names)
            if parent in moves and child in moves:
                modules[moves[child]]='::'.join((module(moves[parent], modules),)+names)
    return modules


def file_mapping(old, new, moves, mapping, rewrite_roots):
    """Select only the reviewed file pair and rewrite roots; paths stay rooted.

    Consumers resolve crate/self/super relative to this pair, never by making
    a global crate:: alias for the source crate's mappings.
    """
    roots = tuple(safe_path(x) + '/' for x in rewrite_roots)
    if not new.endswith('.rs') or not new.startswith(roots): return None
    if moves.get(old, old) != new: return None
    return mapping

def rewrite_rust(text, old_path, new_path, mapping, moves):
    offsets,scopes=inline_modules(text)
    def scope_at(pos): return scopes[bisect.bisect_right(offsets,pos)-1]
    old_mod=module(old_path)
    new_mod=module(new_path)
    old_root=old_path.split('/')[1].replace('-','_') if old_path.startswith('crates/') else None
    new_root=new_path.split('/')[1].replace('-','_') if new_path.startswith('crates/') else None
    # lib.rs path aliases already present in P0.
    aliases = CONFIG.get('aliases', {})
    keys=sorted(mapping,key=lambda k: (-len(k), k))
    def replace(path, inline=()):
        original=path
        suffix=''
        if ' as ' in path: path,suffix=path.split(' as ',1); suffix=' as '+suffix
        self_item=path.endswith('::self')
        if self_item: path=path[:-6]
        if path.startswith(('$crate::','crate::')):
            root=old_root
            # Examples, integration tests and bins have their own crate root.
            if not old_root or '/src/' not in old_path or '/src/bin/' in old_path: return original
            path=root+'::'+path.split('::',1)[1]
        elif path.startswith(('super::','self::')):
            if not old_mod: return original
            # Inline test modules have a different lexical parent. Keep these
            # references for the snapshot residual instead of guessing scope.
            parent=old_mod.split('::')+list(inline)
            while path.startswith('super::'):
                parent=parent[:-1]; path=path[7:]
            if path.startswith('self::'): path=path[6:]
            path='::'.join(parent+[path])
        for a,b in aliases.items():
            if path==a or path.startswith(a+'::'): path=b+path[len(a):]; break
        mapped=path
        for key in keys:
            if path==key or path.startswith(key+'::') or path==key+'!':
                mapped=mapping[key]+path[len(key):]; break
        if mapped==path and old_root != new_root and path.startswith((old_root or '')+'::') and not original.startswith(('super::','self::')):
            return mapped+suffix
        if mapped==path:
            if original.startswith(('super::','self::')) and old_root != new_root and old_mod and not path.endswith('::*'):
                mapped='crate::'+path.split('::',1)[1]
                return mapped+suffix
            return original
        if original.startswith(('super::','self::')) and new_mod and not self_item:
            parent=new_mod.split('::')+list(inline); tail=original.split(' as ',1)[0]
            while tail.startswith('super::'):
                parent=parent[:-1]; tail=tail[7:]
            if tail.startswith('self::'): tail=tail[6:]
            if mapped=='::'.join(parent+[tail]): return original
            # The runtime's existing root imports remain its lexical surface;
            # the inventory also lists canonical owners for external callers.
            old_parent=old_mod.split('::')+list(inline)
            old_tail=original.split(' as ',1)[0]
            while old_tail.startswith('super::'):
                old_parent=old_parent[:-1]; old_tail=old_tail[7:]
            old_parent='::'.join(old_parent)
            if old_tail in ('PresetIo','assert_manifest_gate') and mapping.get(old_parent)=='::'.join(parent): return original
        if new_root and mapped.startswith(new_root+'::'): mapped='crate'+mapped[len(new_root):]
        if original.startswith('$crate::') and mapped.startswith('crate::'): mapped='$'+mapped
        return mapped+suffix
    def use_match(m):
        indent,vis,body=m.groups()
        if '//' in body or '#' in body: return m[0]
        items=expand_use(body)
        changed=[replace(x,scope_at(m.start())) for x in items]
        if changed==items: return m[0]
        if '{' in body and len(changed) > 1:
            parts = [x.split('::') for x in changed]
            common = []
            for group in zip(*parts):
                if len(set(group)) != 1: break
                common.append(group[0])
            if common and all(len(x) > len(common) for x in parts):
                prefix = '::'.join(common) + '::'
                return indent+(vis or '')+'use '+prefix+'{'+', '.join(x[len(prefix):] for x in changed)+'};'
        body = changed[0] if len(changed) == 1 else '{' + ', '.join(changed) + '}'
        return indent+(vis or '')+'use '+body+';'
    replacements=[]
    masked = code_mask(text)
    use_matches=[m for m in USE.finditer(text) if masked[m.start():m.end()] == text[m.start():m.end()]]
    for match in use_matches: replacements.append((match.start(),match.end(),use_match(match)))
    use_index=0
    roots = {'crate', 'super', 'self', *(key.split('::', 1)[0] for key in mapping)}
    token = re.compile(r'(?<![\w$])(?:\$crate|' + '|'.join(re.escape(x) for x in sorted(roots)) + r')(?:::[A-Za-z_]\w*)+(?:!)?')
    for match in token.finditer(masked):
        while use_index<len(use_matches) and use_matches[use_index].end()<=match.start(): use_index+=1
        if use_index<len(use_matches) and use_matches[use_index].start()<=match.start()<use_matches[use_index].end(): continue
        replacements.append((match.start(),match.end(),replace(match[0],scope_at(match.start()))))
    for start,end,value in sorted(replacements,reverse=True): text=text[:start]+value+text[end:]
    # Relative literal asset paths are resolved against the original file.
    def asset(m):
        old_target=os.path.normpath(str(PurePosixPath(old_path).parent/m[2]))
        if old_target not in moves: return m[0]
        target=moves.get(old_target,old_target)
        rel=os.path.relpath(target,str(PurePosixPath(new_path).parent))
        return m[1]+rel+m[3]
    text=code_sub(r'((?:include_str!|include_bytes!)\(\s*")([^"]+)("\s*\))',asset,text)
    return text

def files(root):
    """Git-shaped inventory: bytes, executable bit, and symlink targets."""
    result = {}
    for directory, dirs, names in os.walk(root, followlinks=False):
        dirs.sort()
        names.sort()
        for name in list(dirs):
            p = Path(directory) / name
            if p.is_symlink():
                names.append(name)
                dirs.remove(name)
        for name in sorted(names):
            p = Path(directory) / name
            rel = p.relative_to(root).as_posix()
            mode = p.lstat().st_mode
            if stat.S_ISLNK(mode):
                result[rel] = ('120000', os.fsencode(os.readlink(p)))
            elif stat.S_ISREG(mode):
                result[rel] = ('100755' if mode & 0o111 else '100644', p.read_bytes())
            else:
                raise ValueError('unsupported file type: ' + rel)
    return result


def rust_path(value):
    value = value.removesuffix('::')
    ident = r'(?:r#)?[A-Za-z_][A-Za-z_0-9]*'
    if not re.fullmatch(r'(?:\$crate|' + ident + r')(?:::' + ident + r')*(?:!)?', value):
        raise ValueError('invalid Rust path rewrite: ' + value)
    return value


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


def _testkit_outer_span(text, start, end):
    """Return the smallest enclosing testkit_visible! span for an item, if any."""
    containing = [(call_end - call_start, call_start, call_end)
                 for call_start, call_end, selected_start, selected_end, _dual in _testkit_calls(text)
                 if selected_start <= start and end <= selected_end]
    if not containing:
        return None
    _, call_start, call_end = min(containing)
    return call_start, call_end


def code_sub(pattern, replacement, text):
    masked = code_mask(text)
    return re.sub(pattern, lambda m: replacement(m) if masked[m.start()] == text[m.start()] else m[0], text)


def validate_paths(entries):
    """Reject aliases at every component, including directories and file parents."""
    seen = {}
    leaves = set(entries)
    for rel in sorted(entries):
        safe_path(rel)
        parts = PurePosixPath(rel).parts
        for n in range(1, len(parts)+1):
            path = '/'.join(parts[:n])
            key = unicodedata.normalize('NFD', path.casefold())
            if key in seen and seen[key] != path:
                raise ValueError('path collision: ' + seen[key] + ' / ' + path)
            seen[key] = path
            if n < len(parts) and path in leaves:
                raise ValueError('non-directory ancestor: ' + path)


def no_symlink_parents(path):
    for parent in (path, *path.parents):
        if parent.is_symlink(): raise ValueError('symlink component: ' + str(parent))


def write_regular(path, data, exclusive=False):
    no_symlink_parents(path)
    flags = os.O_WRONLY | os.O_NOFOLLOW
    flags |= os.O_CREAT | os.O_EXCL if exclusive else 0
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode): raise ValueError('non-regular output: ' + str(path))
        stream.truncate(0)
        stream.write(data)


def write_files(root, entries):
    validate_paths(entries)
    no_symlink_parents(root)
    root.mkdir(parents=True, exist_ok=True)
    for rel in sorted(entries):
        p = root / rel
        no_symlink_parents(p)
        p.parent.mkdir(parents=True, exist_ok=True)
        mode, data = entries[rel]
        if mode == '120000':
            p.symlink_to(os.fsdecode(data))
        elif mode in ('100644', '100755'):
            write_regular(p, data, exclusive=True)
            p.chmod(0o755 if mode == '100755' else 0o644)
        else:
            raise ValueError('unsupported Git mode: ' + mode + ' ' + rel)


def git(repo, *args):
    proc = subprocess.run(['git', '-C', str(repo), *args], capture_output=True)
    if proc.returncode:
        raise ValueError(proc.stderr.decode('utf-8', errors='replace').strip())
    return proc.stdout


def tree(repo, revision):
    rows = []
    for row in git(repo, 'ls-tree', '-rz', '--full-tree', revision).split(b'\0'):
        if not row: continue
        meta, path = row.split(b'\t', 1)
        mode, kind, oid = meta.decode('ascii').split()
        if kind != 'blob': raise ValueError('unsupported Git entry: ' + os.fsdecode(path))
        rows.append((os.fsdecode(path), mode, oid))
    process = subprocess.run(['git', '-C', str(repo), 'cat-file', '--batch'],
                             input=''.join(oid+'\n' for _, _, oid in rows).encode(), capture_output=True)
    if process.returncode: raise ValueError('git cat-file batch failed')
    data = process.stdout
    entries = {}
    offset = 0
    for path, mode, oid in rows:
        end = data.index(b'\n', offset)
        actual_oid, kind, size = data[offset:end].decode('ascii').split()
        if actual_oid != oid or kind != 'blob': raise ValueError('unexpected Git batch entry')
        offset = end + 1
        count = int(size)
        entries[path] = (mode, data[offset:offset+count])
        offset += count + 1
    return entries


def checked_file(root, rel):
    safe_path(rel)
    p = root / rel
    if p.is_symlink() or not p.is_file() or not p.resolve().is_relative_to(root.resolve()):
        raise ValueError('missing/non-regular input: ' + rel)
    return p


def manifest_rows(plan):
    path = plan / 'manifests.json'
    if not path.exists(): return []
    rows = json.loads(read_utf8(path))
    for row in rows:
        if set(row) != {'path', 'before', 'after'} or not all(isinstance(v, str) for v in row.values()):
            raise ValueError('invalid manifest hunk')
        safe_path(row['path'])
        if PurePosixPath(row['path']).name not in ('Cargo.toml', 'Cargo.lock'):
            raise ValueError('manifest hunk must target Cargo.toml or Cargo.lock')
    return rows


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


def mount_parent(path, inventory):
    rel = PurePosixPath(path)
    directory = rel.parent.parent if rel.name == 'mod.rs' else rel.parent
    name = rel.parent.name if rel.name == 'mod.rs' else rel.stem
    stem = rel.parent if rel.name == 'mod.rs' else rel.with_suffix('')
    if str(stem)+'.rs' in inventory and str(stem/'mod.rs') in inventory:
        raise ValueError(path + ': ambiguous module source files')
    if directory.name == 'src':
        candidates = [str(directory / 'lib.rs'), str(directory / 'main.rs')]
    else:
        candidates = [str(directory) + '.rs', str(directory / 'mod.rs')]
    parents = [p for p in candidates if p in inventory and inventory[p][0] != '120000']
    if len(parents) != 1:
        raise ValueError(path + ': module parent must exist exactly once')
    return parents[0], name


def mount_items(text, name):
    matches = []
    for start, end, header, scope in module_items(text):
        if scope: continue
        head = text[header:end]
        if not re.match(VIS + r'mod ' + re.escape(name) + r'\b', head): continue
        if not re.fullmatch(VIS + r'mod ' + re.escape(name) + ';', head):
            raise ValueError(name + ': inline or unsupported module mount')
        if _testkit_outer_span(text, start, end):
            raise ValueError(name + ': testkit_visible! module mounts require a separate reviewed fix')
        attrs = text[start:header]
        if re.search(r'\bpath\b', code_mask(attrs)):
            raise ValueError(name + ': path module mounts are forbidden')
        first = text.rfind('\n', 0, start) + 1
        last = text.find('\n', end)
        last = len(text) if last < 0 else last + 1
        if text[first:start].strip() or text[end:last].strip():
            raise ValueError(name + ': module mount must occupy complete lines')
        # Doc comments are attributes too; the lexer masks them, so fail closed.
        previous = text[:first].rstrip()
        if previous.endswith('*/') or (previous and previous.split('\n')[-1].lstrip().startswith('///')):
            raise ValueError(name + ': comment-attached mount requires a separate reviewed fix')
        matches.append((first, last, text[first:last]))
    if len(matches) == 2:
        predicates = []
        for _, _, item in matches:
            # Ordinary mounts in one lexical parent resolve to the same file.
            # Path/cfg_attr(path) mounts were rejected above. Fail closed on
            # additional conditional attributes rather than infer their meaning.
            header = next(header for _, _, header, scope in module_items(item) if not scope)
            cfg = re.fullmatch(r'\s*#\[cfg\((.*)\)\]\s*', item[:header], re.S)
            if not cfg:
                raise ValueError(name + ': paired mounts require one complementary cfg each')
            tokens = re.findall(r'"(?:\\.|[^"\\])*"|[A-Za-z_]\w*|[^\s]', cfg[1])
            predicates.append(tokens)
        a, b = predicates
        if not (a == ['not', '('] + b + [')'] or b == ['not', '('] + a + [')']):
            raise ValueError(name + ': paired mounts require complementary cfg predicates')
    elif len(matches) != 1:
        raise ValueError(name + ': module mount must exist exactly once or as a complementary pair')
    return matches


def derive_mounts(source, templates, moves):
    """Only move-derived module items may leave or enter existing source files."""
    final = {moves.get(p, p): entry for p, entry in source.items()}
    final.update(templates)
    removals = {}; additions = {}
    for old, new in sorted(moves.items()):
        if not module(old): continue
        if PurePosixPath(old).name in ('lib.rs', 'main.rs'):
            raise ValueError(old + ': moving crate roots requires a separate reviewed fix')
        if old in MODULES:
            raise ValueError(old + ': path/include module mounts are forbidden')
        parent, name = mount_parent(old, source)
        items = mount_items(source[parent][1].decode('utf-8'), name)
        new_parent, new_name = mount_parent(new, final)
        # The move determines any identifier rename; visibility and attributes are bytes.
        renamed = []
        for _, _, item in items:
            at = item.rindex('mod ' + name + ';')
            renamed.append(item[:at] + 'mod ' + new_name + ';' + item[at+len('mod ' + name + ';'):])
        if moves.get(parent, parent) == new_parent and name == new_name:
            continue
        removals.setdefault(parent, []).extend((start, end) for start, end, _ in items)
        if new_parent in templates:
            mounted = mount_items(templates[new_parent][1].decode('utf-8'), new_name)
            if [item for _, _, item in mounted] != renamed:
                raise ValueError(new_parent + ': template mount differs from moved item')
        else:
            text = final[new_parent][1].decode('utf-8')
            for _, end2, header, scope in module_items(text):
                if not scope and re.match(VIS + r'mod ' + re.escape(new_name) + r'\b', text[header:end2]):
                    raise ValueError(new_parent + ': destination module already mounted')
            additions.setdefault(new_parent, []).extend(renamed)
    return removals, additions


def bin_targets(entries, crate):
    """Discover only Cargo's two conventional bin shapes, without invoking Cargo."""
    manifest = crate + '/Cargo.toml'
    if manifest not in entries: return {}
    data = tomllib.loads(entries[manifest][1].decode('utf-8'))
    explicit = data.get('bin', [])
    targets = {}
    paths = set()
    def add(name, path, declaration):
        safe_path(path)
        if name in targets or path in paths:
            raise ValueError(manifest + ': colliding bin target: ' + name)
        targets[name] = (path, declaration)
        paths.add(path)
    for row in explicit:
        name = row['name']
        path = row.get('path')
        if path is None:
            candidates = [p for p in (f'src/bin/{name}.rs', f'src/bin/{name}/main.rs')
                          if crate + '/' + p in entries]
            if len(candidates) != 1:
                raise ValueError(manifest + ': bin path must be explicit: ' + name)
            path = candidates[0]
        add(name, crate + '/' + safe_path(path), row)
    if data.get('package', {}).get('autobins', True):
        prefix = crate + '/src/bin/'
        for path in sorted(entries):
            if not path.startswith(prefix): continue
            rel = path[len(prefix):].split('/')
            if len(rel) == 1 and rel[0].endswith('.rs'): name = rel[0][:-3]
            elif len(rel) == 2 and rel[1] == 'main.rs': name = rel[0]
            else: continue
            if path in paths: continue
            add(name, path, {'name': name})
    return targets


def validate_bin_moves(source, templates, moves, plan):
    """A bin moves whole, retaining its target name and manifest configuration."""
    bin_moves = {a: b for a, b in moves.items() if '/src/bin/' in a or '/src/bin/' in b}
    if not bin_moves: return
    manifest_source = dict(source)
    for row in manifest_rows(plan):
        mode, raw = manifest_source[row['path']]
        text = raw.decode('utf-8')
        if not row['before'] or text.count(row['before']) != 1:
            raise ValueError(row['path'] + ': wiring context changed in manifests.json')
        manifest_source[row['path']] = (mode, text.replace(row['before'], row['after'], 1).encode())
    final = {moves.get(p, p): entry for p, entry in manifest_source.items()}
    final.update(templates)
    covered = set()
    crates = {a.split('/src/bin/', 1)[0] for a in bin_moves if '/src/bin/' in a}
    for crate in sorted(crates):
        for name, (root, declaration) in bin_targets(source, crate).items():
            if root not in bin_moves: continue
            new = bin_moves[root]
            if '/src/bin/' not in new:
                raise ValueError(root + ': bin destination must be under src/bin')
            destination = new.split('/src/bin/', 1)[0]
            target = bin_targets(final, destination).get(name)
            if target is None or target[0] != new:
                raise ValueError(new + ': bin target name changed or collided: ' + name)
            # A moved bin must be explicitly carried by the reviewed manifest.
            if target[1].get('path') != new[len(destination)+1:]:
                raise ValueError(new + ': moved bin requires an explicit manifest path')
            old_options = {k: v for k, v in declaration.items() if k != 'path'}
            new_options = {k: v for k, v in target[1].items() if k != 'path'}
            if old_options != new_options:
                raise ValueError(new + ': bin target configuration changed')
            if root.endswith('/main.rs'):
                prefix = root[:-len('main.rs')]
                new_prefix = new[:-len('main.rs')] if new.endswith('/main.rs') else ''
                if not new_prefix:
                    raise ValueError(root + ': directory bin must remain a directory bin')
                members = [p for p in source if p.startswith(prefix)]
                for member in members:
                    if moves.get(member) != new_prefix + member[len(prefix):]:
                        raise ValueError(root + ': directory bin must move whole: ' + member)
                covered.update(members)
            else:
                if new.endswith('/main.rs'):
                    raise ValueError(root + ': single-file bin must remain single-file')
                covered.add(root)
            if crate != destination and crate + '/Cargo.toml' in final:
                if name in bin_targets(final, crate):
                    raise ValueError(root + ': source manifest retains moved bin: ' + name)
    if set(bin_moves) - covered:
        raise ValueError('unowned or partial bin move: ' + ', '.join(sorted(set(bin_moves) - covered)))


def remove_mounts(dest, removals):
    for parent, spans in sorted(removals.items()):
        p = checked_file(dest, parent); text = read_utf8(p)
        for start, end in sorted(spans, reverse=True): text = text[:start] + text[end:]
        write_regular(p, text.encode('utf-8'))


def add_mounts(dest, additions):
    for parent, items in sorted(additions.items()):
        p = checked_file(dest, parent); text = read_utf8(p)
        if text and not text.endswith('\n'):
            raise ValueError(parent + ': cannot append mount after unterminated line')
        # Insert after complete items, before any trailing outer attributes.
        # Prepending would put crate-level inner attributes after the new item.
        ends = [(_testkit_outer_span(text, start, end) or (start, end))[1]
                for start, end, _, scope in module_items(text) if not scope]
        at = max(ends, default=0)
        if at:
            newline = text.find('\n', at)
            at = len(text) if newline < 0 else newline+1
        elif code_mask(text).strip():
            raise ValueError(parent + ': no proven insertion point for module mount')
        write_regular(p, (text[:at] + ''.join(items) + text[at:]).encode('utf-8'))


def review_digest(plan):
    for rel, (mode, data) in sorted(files(plan / 'templates').items()):
        print(f'review template {rel} mode={mode} sha256={hashlib.sha256(data).hexdigest()}')
    for n, row in enumerate(manifest_rows(plan), 1):
        data = json.dumps(row, sort_keys=True, ensure_ascii=True, separators=(',', ':')).encode('utf-8')
        print(f"review manifest {row['path']} hunk={n} sha256={hashlib.sha256(data).hexdigest()} {data.decode('utf-8')}")


def apply_manifests(dest, plan):
    for row in manifest_rows(plan):
        p = checked_file(dest, row['path'])
        old, new = row['before'], row['after']
        text = p.read_bytes().decode('utf-8')
        if not old or text.count(old) != 1:
            raise ValueError(row['path'] + ': wiring context changed in manifests.json')
        write_regular(p, text.replace(old, new, 1).encode('utf-8'))


def replay_tree(source, plan, dest):
    no_symlink_parents(dest)
    if dest.exists(): raise ValueError('destination must not exist')
    try:
        _replay_tree(source, plan, dest)
    except BaseException:
        if dest.exists(): shutil.rmtree(dest)
        raise


def _replay_tree(source, plan, dest):
    global R, E, CONFIG
    CONFIG = json.loads(read_utf8(plan / 'plan.json'))
    if set(CONFIG) - {'version', 'source_crate', 'destination_crate', 'rewrite_roots', 'aliases'}:
        raise ValueError('unsupported plan configuration (body edits require a separate commit)')
    for rel in files(plan):
        if rel not in ('plan.json', 'moves.tsv', 'rewrites.tsv', 'manifests.json', 'README.md',
                       'residual-files.txt', 'residual-deleted.txt') and not rel.startswith(('templates/', 'after/')):
            raise ValueError('unsupported plan file: ' + rel)
    for a, b in CONFIG.get('aliases', {}).items(): rust_path(a); rust_path(b)
    manifest_rows(plan)
    if CONFIG.get('version') != 1: raise ValueError('unsupported plan version')
    R = safe_path(CONFIG['source_crate']) + '/'
    E = safe_path(CONFIG['destination_crate']) + '/'
    if dest.exists(): raise ValueError('destination must not exist')
    if source == dest or source.is_relative_to(dest): raise ValueError('invalid destination')
    moves = {}
    destinations = set()
    for a, b in tsv(plan / 'moves.tsv'):
        safe_path(a); safe_path(b)
        if any(c in a+b for c in ('"', "'", '\n', '\r', '\0')):
            raise ValueError('unsafe file path rewrite: ' + a + ' -> ' + b)
        if a in moves or b in destinations: raise ValueError('duplicate move: ' + a + ' -> ' + b)
        checked_file(source, a)
        if (source / b).exists() or (source / b).is_symlink():
            raise ValueError('move destination exists: ' + b)
        moves[a] = b
        destinations.add(b)
    source_entries = files(source)
    template_entries = files(plan / 'templates')
    validate_paths(set(source_entries) | set(moves.values()) | set(template_entries))
    validate_bin_moves(source_entries, template_entries, moves, plan)
    collect_path_modules(source, moves)
    mapping = mappings(moves, tsv(plan / 'rewrites.tsv'))
    removals, additions = derive_mounts(source_entries, template_entries, moves)
    # Source is a validated, materialized Git tree or explicit draft directory.
    write_files(dest, source_entries)
    apply_manifests(dest, plan)
    remove_mounts(dest, removals)
    for a, b in sorted(moves.items()):
        p = dest / b
        no_symlink_parents(p)
        p.parent.mkdir(parents=True, exist_ok=True)
        (dest / a).rename(p)
    inverse = {b: a for a, b in moves.items()}
    for p in sorted(dest.rglob('*.rs')):
        if p.is_symlink(): continue
        new = p.relative_to(dest).as_posix()
        old = inverse.get(new, new)
        selected = file_mapping(old, new, moves, mapping, CONFIG['rewrite_roots'])
        if selected is None: continue
        write_regular(p, rewrite_rust(read_utf8(p), old, new, selected, moves).encode('utf-8'))
    templates = plan / 'templates'
    if templates.exists():
        for rel, entry in sorted(files(templates).items()):
            if (dest / rel).exists() or (dest / rel).is_symlink():
                raise ValueError('template would overwrite input: ' + rel)
            write_files(dest, {rel: entry})
    add_mounts(dest, additions)
    validate_paths(files(dest))
    print(f'replayed {len(moves)} moves, {len(mapping)} path mappings')


def plan_in_tree(plan, repo, entries):
    try: rel = plan.relative_to(repo).as_posix()
    except ValueError: raise ValueError('plan must be inside repository') from None
    safe_path(rel)
    supplied = files(plan)
    if not supplied: raise ValueError('empty plan')
    if any(mode == '120000' for mode, _ in supplied.values()):
        raise ValueError('plan inputs must not be symlinks')
    expected = {p[len(rel)+1:]: v for p, v in entries.items() if p.startswith(rel + '/')}
    return rel, supplied, expected


def differences(expected, actual):
    return [p for p in sorted(expected.keys() | actual.keys()) if expected.get(p) != actual.get(p)]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    verbs = parser.add_subparsers(dest='verb', required=True)
    run = verbs.add_parser('replay', help='replay HEAD (or --source tree) using a reviewed plan')
    run.add_argument('--plan', type=Path, required=True)
    run.add_argument('--dest', type=Path, required=True)
    run.add_argument('--source', help='Git revision or draft directory to replay; default HEAD', default='HEAD')
    check = verbs.add_parser('verify', help='compare replay of parent to entire commit tree')
    check.add_argument('--plan', type=Path, required=True)
    check.add_argument('commit')
    args = parser.parse_args(argv)
    try:
        plan = args.plan.resolve()
        repo = Path(git(plan, 'rev-parse', '--show-toplevel').decode('utf-8').strip()).resolve()
        revision = args.commit if args.verb == 'verify' else args.source
        if args.verb == 'replay' and Path(revision).is_dir():
            directory = Path(revision).absolute()
            no_symlink_parents(directory)
            directory = directory.resolve()
            output = args.dest.absolute()
            no_symlink_parents(output)
            output = output.resolve()
            if directory == output or directory.is_relative_to(output) or output.is_relative_to(directory):
                raise ValueError('draft source and destination must be disjoint')
            committed = files(directory)
        else:
            # Commit verification always requires a Git commit, never a directory.
            oid = git(repo, 'rev-parse', '--verify', revision + '^{commit}').decode('utf-8').strip()
            committed = tree(repo, oid)
        validate_paths(committed)
        rel, supplied, expected_plan = plan_in_tree(plan, repo, committed)
        if args.verb == 'verify':
            parents = git(repo, 'rev-list', '--parents', '-n', '1', oid).decode('ascii').split()[1:]
            if len(parents) != 1: raise ValueError('verify requires exactly one parent')
            base = tree(repo, parents[0])
            parent_plan = {p[len(rel)+1:]: v for p, v in base.items() if p.startswith(rel + '/')}
            if not parent_plan: raise ValueError('plan must be committed and reviewed before the move')
            drift = differences(parent_plan, expected_plan)
            if drift: raise ValueError('move commit changes plan: ' + ', '.join(drift[:20]))
            drift = differences(parent_plan, supplied)
            if drift: raise ValueError('plan differs from commit parent: ' + ', '.join(drift[:20]))
        else:
            base = committed
        validate_paths(base)
        scratch = repo / 'target'
        no_symlink_parents(scratch)
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='crate-move-', dir=scratch) as temporary:
            tmp = Path(temporary)
            source = tmp / 'parent'
            write_files(source, base)
            if differences(base, files(source)): raise ValueError('parent tree materialization differs from source inventory')
            if args.verb == 'replay':
                # Draft replay uses the supplied plan; residual artifacts stay inert.
                draft_plan = tmp / 'plan'
                write_files(draft_plan, supplied)
                # Include draft plan bytes in output for inspection before committing it.
                for p in list(base):
                    if p.startswith(rel + '/'): del base[p]
                for p, value in supplied.items(): base[rel + '/' + p] = value
                shutil.rmtree(source)
                write_files(source, base)
                output = args.dest.absolute()
                replay_tree(source, draft_plan, output)
            else:
                output = tmp / 'replayed'
                review_digest(source / rel)
                replay_tree(source, source / rel, output)
            if args.verb == 'verify':
                drift = differences(committed, files(output))
                if drift:
                    for path in drift[:20]: print('different: ' + path)
                    print(f'{len(drift)} differing paths')
                    return 1
                print('identical: replay matches the complete commit tree')
        return 0
    except (ValueError, OSError, KeyError, UnicodeError) as error:
        print('crate-move: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
