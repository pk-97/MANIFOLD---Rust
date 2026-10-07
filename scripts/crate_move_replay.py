#!/usr/bin/env python3
"""Deterministically replay reviewed crate moves and verify whole Git tree identity."""
import argparse
import bisect
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import tempfile
import sys

CONFIG = {}
R = 'crates/manifold-renderer/'
E = 'crates/manifold-node-engine/'
MODULES = {}

def tsv(path):
    rows = []
    for n, line in enumerate(Path(path).read_text().splitlines(), 1):
        if not line.strip() or line.startswith('#'): continue
        cells = line.split('\t')
        if len(cells) != 2 or not all(cells): raise ValueError(f'{path}:{n}: expected two nonempty tab-separated columns')
        rows.append(tuple(cells))
    return rows

def safe_path(p):
    q = PurePosixPath(p)
    if q.is_absolute() or '..' in q.parts or '.git' in q.parts or 'target' in q.parts:
        raise ValueError(f'unsafe relative path: {p}')
    return p

def module(path):
    if path in MODULES: return MODULES[path]
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

def mappings(moves, rewrites):
    result = {a.removesuffix('::'): b.removesuffix('::') for a, b in rewrites}
    for a, b in moves.items():
        if module(a) and module(b):
            result[module(a)] = module(b)
    return result

def inline_modules(source):
    token=re.compile(r'/\*.*?\*/|//[^\n]*|r(\#*)".*?"\1|"(?:\\.|[^"\\])*"|\bmod\s+([A-Za-z_]\w*)\s*\{|[{}]', re.S)
    offsets=[0]; scopes=[()]; stack=[]; modules=[]
    for match in token.finditer(source):
        item=match[0]
        if item.startswith(('//','/*','"','r"','r#')): continue
        if match[2]: stack.append(True); modules.append(match[2])
        elif item=='{': stack.append(False)
        elif stack and stack.pop(): modules.pop()
        offsets.append(match.end()); scopes.append(tuple(modules))
    return offsets,scopes

def collect_path_modules(source,moves):
    MODULES.clear()
    rows=[]
    for p in sorted((source/(R+'src')).rglob('*.rs')):
        text=p.read_text(); parent=str(p.relative_to(source))
        offsets,scopes=inline_modules(text)
        for m in re.finditer(r'#\[path\s*=\s*"([^"]+)"\]\s*(?:#\[[^\n]+\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;',text):
            child=os.path.normpath(str(PurePosixPath(parent).parent/m[1]))
            if (source/child).is_file():
                inline=scopes[bisect.bisect_right(offsets,m.start())-1]
                rows.append((parent,child,inline+(m[2],)))
        for m in re.finditer(r'include!\("([^"]+\.rs)"\)',text):
            child=os.path.normpath(str(PurePosixPath(parent).parent/m[1]))
            if (source/child).is_file():
                rows.append((parent,child,scopes[bisect.bisect_right(offsets,m.start())-1]))
    for _ in range(4):
        for parent,child,names in rows:
            if module(parent): MODULES[child]='::'.join((module(parent),)+names)
            if parent in moves and child in moves:
                MODULES[moves[child]]='::'.join((module(moves[parent]),)+names)

def rewrite_rust(text, old_path, new_path, mapping, moves):
    for match in list(re.finditer(r'(?m)^use (manifold_renderer::node_graph) as (\w+);\n',text)):
        prefix,alias=match.groups()
        uses=re.findall(r'\b'+re.escape(alias)+r'::([A-Za-z_]\w*)',text)
        if uses and all(prefix+'::'+item in mapping for item in uses):
            text=text.replace(match[0],'')
            text=re.sub(r'\b'+re.escape(alias)+r'::',prefix+'::',text)
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
        return '\n'.join(indent+(vis or '')+'use '+x+';' for x in changed)
    replacements=[]
    use_matches=list(USE.finditer(text))
    for match in use_matches: replacements.append((match.start(),match.end(),use_match(match)))
    use_index=0
    roots = {'crate', 'super', 'self', *(key.split('::', 1)[0] for key in mapping)}
    token = re.compile(r'(?<![\w$])(?:\$crate|' + '|'.join(re.escape(x) for x in sorted(roots)) + r')(?:::[A-Za-z_]\w*)+(?:!)?')
    for match in token.finditer(text):
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
    text=re.sub(r'((?:include_str!|include_bytes!)\(\s*")([^"]+)("\s*\))',asset,text)
    def path_attr(m):
        target=os.path.normpath(str(PurePosixPath(old_path).parent/m[1]))
        if target not in moves:return m[0]
        dest_target=moves[target]
        standard=str(PurePosixPath(new_path).with_suffix('')/m[3])+'.rs' if not new_path.endswith(('mod.rs','lib.rs')) else str(PurePosixPath(new_path).parent/(m[3]+'.rs'))
        if dest_target==standard:
            if old_path==R+'src/generators/mesh_common.rs': return m[2]+'mod '+m[3]+';'
            return m[0]
        if not dest_target.startswith(new_path.split('/src/')[0]+'/src/'):
            return 'use '+module(dest_target)+' as '+m[3]+';'
        rel=os.path.relpath(dest_target,str(PurePosixPath(new_path).parent))
        return '#[path = "'+rel+'"]\n'+m[2]+'mod '+m[3]+';'
    text=re.sub(r'#\[path = "([^"]+)"\]\n((?:pub(?:\([^)]*\))? )?)mod (\w+);',path_attr,text)
    if old_path==R+'src/lib.rs':
        text=text.replace('inventory::submit!(preset_loader::','inventory::submit!(manifold_node_engine::load::preset_loader::')
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


def write_files(root, entries):
    root.mkdir(parents=True, exist_ok=True)
    # Symlink ancestors are rejected, never followed during materialization.
    for rel in sorted(entries):
        safe_path(rel)
        p = root / rel
        for parent in p.parents:
            if parent == root: break
            if parent.is_symlink(): raise ValueError('symlink ancestor: ' + rel)
        p.parent.mkdir(parents=True, exist_ok=True)
        mode, data = entries[rel]
        if mode == '120000':
            p.symlink_to(os.fsdecode(data))
        elif mode in ('100644', '100755'):
            p.write_bytes(data)
            p.chmod(0o755 if mode == '100755' else 0o644)
        else:
            raise ValueError('unsupported Git mode: ' + mode + ' ' + rel)


def git(repo, *args):
    proc = subprocess.run(['git', '-C', str(repo), *args], capture_output=True)
    if proc.returncode:
        raise ValueError(proc.stderr.decode(errors='replace').strip())
    return proc.stdout


def tree(repo, revision):
    rows = []
    for row in git(repo, 'ls-tree', '-rz', '--full-tree', revision).split(b'\0'):
        if not row: continue
        meta, path = row.split(b'\t', 1)
        mode, kind, oid = meta.decode().split()
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
        actual_oid, kind, size = data[offset:end].decode().split()
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


def patches(dest, plan, name):
    path = plan / name
    if not path.exists(): return
    for row in json.loads(path.read_text()):
        p = checked_file(dest, row['path'])
        old, new = row['before'], row['after']
        text = p.read_bytes().decode('utf-8')
        if not old or text.count(old) != 1:
            raise ValueError(row['path'] + ': wiring context changed in ' + name)
        p.write_bytes(text.replace(old, new, 1).encode('utf-8'))


def replay_tree(source, plan, dest):
    global R, E, CONFIG
    CONFIG = json.loads((plan / 'plan.json').read_text())
    if CONFIG.get('version') != 1: raise ValueError('unsupported plan version')
    R = safe_path(CONFIG['source_crate']) + '/'
    E = safe_path(CONFIG['destination_crate']) + '/'
    if dest.exists(): raise ValueError('destination must not exist')
    if source == dest or source.is_relative_to(dest): raise ValueError('invalid destination')
    moves = {}
    destinations = set()
    for a, b in tsv(plan / 'moves.tsv'):
        safe_path(a); safe_path(b)
        if a in moves or b in destinations: raise ValueError('duplicate move: ' + a + ' -> ' + b)
        checked_file(source, a)
        if (source / b).exists() or (source / b).is_symlink():
            raise ValueError('move destination exists: ' + b)
        moves[a] = b
        destinations.add(b)
    collect_path_modules(source, moves)
    mapping = mappings(moves, tsv(plan / 'rewrites.tsv'))
    # Source is a materialized Git tree, never a checkout with caches or local secrets.
    write_files(dest, files(source))
    patches(dest, plan, 'manifests.json')
    for a, b in sorted(moves.items()):
        p = dest / b
        p.parent.mkdir(parents=True, exist_ok=True)
        if not p.parent.resolve().is_relative_to(dest.resolve()): raise ValueError('symlink move parent: ' + b)
        (dest / a).rename(p)
    inverse = {b: a for a, b in moves.items()}
    roots = tuple(safe_path(x) + '/' for x in CONFIG['rewrite_roots'])
    for p in sorted(dest.rglob('*.rs')):
        if p.is_symlink(): continue
        new = p.relative_to(dest).as_posix()
        if not new.startswith(roots): continue
        old = inverse.get(new, new)
        p.write_bytes(rewrite_rust(p.read_bytes().decode('utf-8'), old, new, mapping, moves).encode('utf-8'))
    patches(dest, plan, 'declarations.json')
    templates = plan / 'templates'
    if templates.exists():
        for rel, entry in sorted(files(templates).items()):
            if (dest / rel).exists() or (dest / rel).is_symlink():
                raise ValueError('template would overwrite input: ' + rel)
            write_files(dest, {rel: entry})
    split = CONFIG.get('split_identity')
    if split:
        build = checked_file(dest, split['renderer_build'])
        text = build.read_text()
        emission = re.compile(r'    native_source_identity::emit_source_identity\(.*?\n    \)\n    \.expect\([^;]+;\n', re.S)
        calls = list(emission.finditer(text))
        integration = [m for m in calls if '"'+split['integration_key']+'"' in m[0]]
        family = [m for m in calls if '"'+split['family_source']+'"' in m[0] and m not in integration]
        if len(integration) != 1 or len(family) != 1:
            raise ValueError('physics identity seam incomplete: expected separate integration and family emissions')
        text = text[:integration[0].start()] + text[integration[0].end():]
        if re.search(r'"\.\./[^"\n]+/src/', text):
            raise ValueError('renderer identity still reads another crate source')
        build.write_text(text)
    patches(dest, plan, 'finish.json')
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
    run.add_argument('--source', help='Git revision to replay; default HEAD', default='HEAD')
    check = verbs.add_parser('verify', help='compare replay of parent to entire commit tree')
    check.add_argument('--plan', type=Path, required=True)
    check.add_argument('commit')
    args = parser.parse_args(argv)
    try:
        plan = args.plan.resolve()
        repo = Path(git(plan, 'rev-parse', '--show-toplevel').decode().strip()).resolve()
        revision = args.commit if args.verb == 'verify' else args.source
        oid = git(repo, 'rev-parse', '--verify', revision + '^{commit}').decode().strip()
        committed = tree(repo, oid)
        rel, supplied, expected_plan = plan_in_tree(plan, repo, committed)
        if args.verb == 'verify':
            drift = differences(expected_plan, supplied)
            if drift: raise ValueError('plan differs from commit: ' + ', '.join(drift[:20]))
            parents = git(repo, 'rev-list', '--parents', '-n', '1', oid).decode().split()[1:]
            if len(parents) != 1: raise ValueError('verify requires exactly one parent')
            base = tree(repo, parents[0])
        else:
            base = committed
        # The plan is a complete reviewed input, including additions and removals.
        for p in list(base):
            if p.startswith(rel + '/'): del base[p]
        for p, value in supplied.items(): base[rel + '/' + p] = value
        scratch = repo / 'target'
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='crate-move-', dir=scratch) as temporary:
            tmp = Path(temporary)
            source = tmp / 'parent'
            write_files(source, base)
            output = args.dest.resolve() if args.verb == 'replay' else tmp / 'replayed'
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
