"""Content-keyed artifact cache (docs/DETECTOR_LAB_DESIGN.md D3), on eval/bundles.py: the key is the song's audio
content (registry.Song.content_id), the stamp names the artifact, its settings and its builder's source. A recycled
song name never reuses another song's spectra; a builder or setting change is a miss. Artifacts keep today's dtypes
(spectra and noisiness float16), so net inputs match the legacy path byte for byte."""
import hashlib
import inspect
from pathlib import Path

import numpy as np

from tools.audio_analysis.eval.bundles import AnalysisBundle, BundleStamp, load_bundle

ROOT = Path.home() / '.cache/manifold/detector-lab'
VERSION = 'detector-lab-1'


def code_id(fn):
    return hashlib.sha256(inspect.getsource(fn).encode()).hexdigest()[:12]


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
