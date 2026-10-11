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


def module_id(*mods):
    """The source of whole modules: a change to any helper the builder calls is a miss."""
    h = hashlib.sha256()
    for m in mods:
        h.update(Path(m.__file__).read_bytes())
    return h.hexdigest()[:12]


def candidates(song, rule, build=True):
    """(candidate hops, emission hops) for a candidate rule ('snare:DB': snare_cands at DB). build=False: a miss is
    an error, so scoring never reads audio."""
    from tools.audio_analysis.eval import detector_cands, snare_cands
    from tools.audio_analysis.lab.registry import mix
    kind, db = rule.split(':')
    if kind != 'snare':
        raise ValueError(f'candidate rule {rule}: only snare:DB until the bass and synth families move (P5)')

    def make():
        if not build:
            raise SystemExit(f'{song.id}: no cached {rule} candidates; run the experiment first')
        return np.stack(snare_cands.candidates(mix(song), song.sample_rate, float(db))).astype(np.int64)
    a = artifact(song, 'candidates', dict(rule=rule, code=module_id(snare_cands, detector_cands)), make)
    return a[0], a[1]
