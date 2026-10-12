"""Content-keyed artifact cache (docs/DETECTOR_LAB_DESIGN.md D3), on eval/bundles.py: the key is the song's audio
content (registry.Song.content_id), the stamp names the artifact, its settings and its builder's source. A recycled
song name never reuses another song's spectra; a builder or setting change is a miss. Artifacts keep today's dtypes
(spectra and noisiness float16), so net inputs match the legacy path byte for byte."""
import ast
import functools
import hashlib
import importlib
import inspect
import os
import re
import textwrap
import types
from pathlib import Path

import numpy as np

# _filename_stem_for: the shared stem rule (content hash + stamp hash); eval/bundles.py owns it so both sides of
# every cache entry stay one key.
from tools.audio_analysis.eval.bundles import AnalysisBundle, BundleStamp, _filename_stem_for, load_bundle

ROOT = Path.home() / '.cache/manifold/detector-lab'
VERSION = 'detector-lab-1'


PACKAGE = 'tools.audio_analysis'
TOOLS = Path(__file__).resolve().parents[1]
REPO = TOOLS.parents[1]


def code_id(fn):
    """A builder's closure: its source, every constant it reads and every helper it calls in tools/audio_analysis
    (followed through globals, module attributes and imports inside the body). Editing any of them is a miss;
    editing unrelated code in the same modules is not."""
    h = hashlib.sha256()
    seen = set()
    home = fn.__module__

    def ours(name):
        return isinstance(name, str) and (name == home or name.startswith(PACKAGE))

    def canon(v):
        """A stable text for a constant; helpers inside containers are followed and named."""
        if isinstance(v, types.ModuleType):
            return f'<module {v.__name__}>'
        if hasattr(v, '__wrapped__') and not isinstance(v, type):
            return canon(inspect.unwrap(v))
        if isinstance(v, (types.FunctionType, type)):
            if ours(getattr(v, '__module__', None)):
                visit(v)
            return f'<{getattr(v, "__module__", "?")}.{v.__qualname__}>'
        if isinstance(v, (bool, int, float, str, bytes)) or v is None:
            return repr(v)
        if isinstance(v, (list, tuple)):
            return f'{type(v).__name__}[' + ','.join(canon(x) for x in v) + ']'
        if isinstance(v, (set, frozenset)):
            return 'set[' + ','.join(sorted(canon(x) for x in v)) + ']'
        if isinstance(v, dict):
            return 'dict[' + ','.join(sorted(f'{canon(k)}:{canon(x)}' for k, x in v.items())) + ']'
        if isinstance(v, np.ndarray):
            return f'array[{v.dtype}{v.shape}{hashlib.sha256(v.tobytes()).hexdigest()}]'
        if isinstance(v, re.Pattern):
            return f'{v.pattern}/{v.flags}'
        if hasattr(v, '__dataclass_fields__'):
            return f'{type(v).__qualname__}(' + ','.join(f'{k}={canon(getattr(v, k))}' for k in v.__dataclass_fields__) + ')'
        if isinstance(v, Path):
            # Paths inside the checkout are named from its root, so every worktree shares one key.
            p = v.resolve()
            return f'path[{p.relative_to(REPO) if p.is_relative_to(REPO) else v}]'
        return f'<{type(v).__module__}.{type(v).__qualname__}>'

    def value(v):
        h.update(canon(v).encode())

    def visit(f):
        # Named by file, not module: run.py is __main__ when run and a module when imported.
        where = Path(inspect.getsourcefile(f)).resolve()
        key = f'{where.relative_to(TOOLS) if where.is_relative_to(TOOLS) else where.name}:{f.__qualname__}'
        if key in seen:
            return
        seen.add(key)
        src = textwrap.dedent(inspect.getsource(f))
        h.update(key.encode() + src.encode())
        env = dict(vars(inspect.getmodule(f)))
        tree = ast.parse(src)
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
                mod = importlib.import_module(node.module)
                for a in node.names:
                    env[a.asname or a.name] = getattr(mod, a.name) if hasattr(mod, a.name) else importlib.import_module(f'{node.module}.{a.name}')
            elif isinstance(node, ast.Import):
                for a in node.names:
                    env[a.asname or a.name.split('.')[0]] = importlib.import_module(a.name if a.asname else a.name.split('.')[0])
        for node in ast.walk(tree):
            if isinstance(node, ast.Name) and node.id in env:
                value(env[node.id])
            elif isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
                mod = env.get(node.value.id)
                if isinstance(mod, types.ModuleType) and ours(mod.__name__) and hasattr(mod, node.attr):
                    value(getattr(mod, node.attr))
    visit(fn)
    return h.hexdigest()[:12]


@functools.lru_cache(maxsize=1)
def loader_id():
    """The audio loader's code: every artifact is built from registry.mix's output."""
    from tools.audio_analysis.lab.registry import mix
    return code_id(mix)


def artifact(song, name, settings, build, root=ROOT / 'artifacts', file_backed=False):
    """The cached array for (song content, name, settings, the audio loader); build() makes it on a miss.
    file_backed (BUG-xd15a): a float32 copy is also kept as a raw .npy beside the bundle and returned as a memory
    map, so Songs hold file-backed pages the kernel evicts without swapping (a legacy .npz hit decompresses into
    anonymous RAM, which only swapping can evict — the corpus's ~15 GB of spectra is what froze the Mac). The stamp
    is unchanged, so an existing bundle stays a hit: its first file-backed read is migrated from the .npz it
    already has, writing the identical float32 (the stored float16 widened). The .npz itself is untouched."""
    stamp = BundleStamp(pipeline_version=f'{VERSION}/{name}',
                        models={k: str(v) for k, v in sorted({**settings, 'loader': loader_id()}.items())})
    npy = None
    if file_backed:
        npy = root / f'{_filename_stem_for(song.content_id, stamp)}.npy'
        if npy.exists():
            # A sidecar at this stem was written for exactly this (content, stamp) key, so it is the hit.
            return np.load(npy, mmap_mode='r+')

        def backed(arr):
            tmp = npy.with_name(npy.stem + '.tmp.npy')  # np.save appends .npy when missing; same guard as save()
            np.save(tmp, arr.astype(np.float32))
            os.replace(tmp, npy)
            return np.load(npy, mmap_mode='r+')
    hit = load_bundle(song.content_id, stamp, root)
    if hit is not None:
        arr = hit.arrays['a']
        return backed(arr) if file_backed else arr
    arr = build()
    AnalysisBundle(content_hash=song.content_id, stamp=stamp, arrays={'a': arr}).save(root)
    return backed(arr) if file_backed else arr


def spectrum(song, root=ROOT / 'artifacts'):
    """The net's 64-band spectrum, as the legacy name-keyed cache returns it (float16 stored, float32 out).
    File-backed (BUG-xd15a): the float32 array is a raw .npy beside the bundle and the Song gets a memory-mapped
    view of it, so it reads the identical bytes the legacy npz path returned without pinning anonymous RAM."""
    from tools.audio_analysis.eval import kick_goal_nn as nn
    from tools.audio_analysis.lab.registry import mix
    a = artifact(song, 'spectrum', dict(bands=nn.BANDS, code=code_id(nn.spectrum)),
                 lambda: nn.spectrum(mix(song), song.sample_rate).astype(np.float16),
                 root=root, file_backed=True)
    return a if a.dtype == np.float32 else a.astype(np.float32)


def flatness(song):
    """Per-band noisiness, as the legacy name-keyed cache returns it."""
    from tools.audio_analysis.eval import detector_channels as dc
    from tools.audio_analysis.lab.registry import mix
    a = artifact(song, 'flatness', dict(code=code_id(dc.flatness)),
                 lambda: dc.flatness(mix(song), song.sample_rate).astype(np.float16))
    return a.astype(np.float32)


def percussive(song):
    """Per-band percussive share (kick_goal_nn.percussive) of the net's own spectrum."""
    from tools.audio_analysis.eval import kick_goal_nn as nn
    a = artifact(song, 'percussive', dict(code=code_id(nn.percussive), spec=code_id(nn.spectrum)),
                 lambda: nn.percussive(spectrum(song)).astype(np.float16))
    return a.astype(np.float32)


def module_id(*mods):
    """The source of whole modules: a change to any helper the builder calls is a miss."""
    h = hashlib.sha256()
    for m in mods:
        h.update(Path(m.__file__).read_bytes())
    return h.hexdigest()[:12]
