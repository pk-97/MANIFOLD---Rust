"""Content-keyed artifact cache (docs/DETECTOR_LAB_DESIGN.md D3), on eval/bundles.py: the key is the song's audio
content (registry.Song.content_id), the stamp names the artifact, its settings and its builder's source. A recycled
song name never reuses another song's spectra; a builder or setting change is a miss. Artifacts keep today's dtypes
(spectra and noisiness float16), so net inputs match the legacy path byte for byte."""
import ast
import hashlib
import importlib
import inspect
import re
import textwrap
import types
from pathlib import Path

import numpy as np

from tools.audio_analysis.eval.bundles import AnalysisBundle, BundleStamp, load_bundle

ROOT = Path.home() / '.cache/manifold/detector-lab'
VERSION = 'detector-lab-1'


PACKAGE = 'tools.audio_analysis'


def code_id(fn):
    """A builder's closure: its source, every constant it reads and every helper it calls in tools/audio_analysis
    (followed through globals, module attributes and imports inside the body). Editing any of them is a miss;
    editing unrelated code in the same modules is not."""
    h = hashlib.sha256()
    seen = set()
    home = fn.__module__

    def ours(name):
        return isinstance(name, str) and (name == home or name.startswith(PACKAGE))

    def value(v):
        if isinstance(v, types.ModuleType):
            return
        if isinstance(v, (types.FunctionType, type)) and ours(getattr(v, '__module__', None)):
            visit(v)
        elif isinstance(v, (bool, int, float, str, bytes, tuple, frozenset)) or v is None:
            h.update(repr(v).encode())
        elif isinstance(v, np.ndarray):
            h.update(str(v.dtype).encode() + v.tobytes())
        elif isinstance(v, re.Pattern):
            h.update(f'{v.pattern}/{v.flags}'.encode())
        elif hasattr(v, '__dataclass_fields__'):
            h.update(repr(v).encode())

    def visit(f):
        key = f'{f.__module__}.{f.__qualname__}'
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


def artifact(song, name, settings, build, root=ROOT / 'artifacts'):
    """The cached array for (song content, name, settings); build() makes it on a miss."""
    stamp = BundleStamp(pipeline_version=f'{VERSION}/{name}', models={k: str(v) for k, v in sorted(settings.items())})
    hit = load_bundle(song.content_id, stamp, root)
    if hit is not None:
        return hit.arrays['a']
    arr = build()
    AnalysisBundle(content_hash=song.content_id, stamp=stamp, arrays={'a': arr}).save(root)
    return arr


def spectrum(song):
    """The net's 64-band spectrum, as the legacy name-keyed cache returns it (float16 stored, float32 out)."""
    from tools.audio_analysis.eval import kick_goal_nn as nn
    from tools.audio_analysis.lab.registry import mix
    a = artifact(song, 'spectrum', dict(bands=nn.BANDS, code=code_id(nn.spectrum)),
                 lambda: nn.spectrum(mix(song), song.sample_rate).astype(np.float16))
    return a.astype(np.float32)


def flatness(song):
    """Per-band noisiness, as the legacy name-keyed cache returns it."""
    from tools.audio_analysis.eval import detector_channels as dc
    from tools.audio_analysis.lab.registry import mix
    a = artifact(song, 'flatness', dict(code=code_id(dc.flatness)),
                 lambda: dc.flatness(mix(song), song.sample_rate).astype(np.float16))
    return a.astype(np.float32)


def module_id(*mods):
    """The source of whole modules: a change to any helper the builder calls is a miss."""
    h = hashlib.sha256()
    for m in mods:
        h.update(Path(m.__file__).read_bytes())
    return h.hexdigest()[:12]
