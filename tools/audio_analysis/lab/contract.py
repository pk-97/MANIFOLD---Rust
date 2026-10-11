"""The model file contract v2 (docs/DETECTOR_RUNTIME_DESIGN.md section 4): entries(record) writes a release record as
container entries, check(entries, parts) refuses exactly what the Rust runtime refuses, each case named. Rust never
parses `config`; it reads only these entries."""
from __future__ import annotations

import json
import re

import numpy as np

from tools.audio_analysis import kick_container

CONTRACT = 2
RATE, HOP = 48000, 256
COMBINE_CLIP = 1e-6  # lab/models.py combine clips each logit here
MAX_WINDOWS = 4
TOP = frozenset({'contract', 'family', 'train_id', 'config', 'run_id', 'rate', 'hop', 'finder', 'models', 'combine',
                 'combine.clip', 'cutoff', 'refractory_s', 'stage'})
FAMILIES = ('kick', 'snare')
MODELS = {'net': ('net',), 'trees': ('trees',), 'net,trees': ('net', 'trees')}
PART_PREFIXES = ('base.', 'cand.', 'extra.', 'fit.', 'flat.', 'perc.', 'kick2.', 'net.', 'trees.', 'stage.')
RESERVED = ('res.', 'glide.')  # res16 and glide3: Deferred


class ContractError(ValueError):
    def __init__(self, case, detail):
        super().__init__(f'{case}: {detail}')
        self.case = case


def text(a):
    return bytes(np.asarray(a, np.uint8)).decode()


def names_of(s):
    return [n for n in s.split(',') if n] if s else []


# ---------------------------------------------------------------- writer

def entries(record):
    """Every entry of the record's model file: the top-level ones, then each part's hook (lab/export/)."""
    from tools.audio_analysis.lab.export import finder_kick, finder_snare, frames, kick2, measures, net, perc, stage, trees
    from tools.audio_analysis.lab.release import measures_of
    i64, f64 = np.int64, np.float64
    out = {
        'contract': np.array(CONTRACT, np.int32),
        'family': kick_container.text(record['family']),
        'train_id': kick_container.text(record['train_id']),
        'config': kick_container.text(json.dumps(record['config'], sort_keys=True)),
        'run_id': kick_container.text(record['run_id']),
        'rate': np.array(RATE, i64),
        'hop': np.array(HOP, i64),
        'finder': kick_container.text(record['finder']),
        'models': kick_container.text(','.join(m for m in ('net', 'trees') if record[m])),
        'combine': kick_container.text(record['combine']),
        'combine.clip': np.array(COMBINE_CLIP, f64),
        'cutoff': np.array(float(record['cutoff']), f64),
        'refractory_s': np.array(float(record['refractory_s']), f64),
        'stage': np.array(1 if record['stage'] else 0, np.int32),
    }
    hooks = [finder_kick if record['finder'] == 'kick' else finder_snare, measures]
    if record['net']:
        hooks += [net] + ([frames] if 'flat' in record['net']['channels'] else []) \
            + ([perc] if 'perc' in record['net']['channels'] else [])
    if 'kick2' in measures_of(record):
        hooks.append(kick2)
    if record['trees']:
        hooks.append(trees)
    if record['stage']:
        hooks.append(stage)
    owner = {}
    for hook in hooks:
        prefixes = getattr(hook, 'PREFIXES', None) or (hook.PREFIX,)
        for name, a in hook.entries(record).items():
            a = np.asarray(a)
            if not name.startswith(prefixes):
                raise ValueError(f'{hook.__name__}: entry {name!r} must start with one of {prefixes}')
            if a.dtype not in kick_container.CODES:
                raise ValueError(f'{hook.__name__}: entry {name!r} has dtype {a.dtype}, not a container dtype')
            if name in out:
                raise ValueError(f'entry {name!r} comes from both {owner.get(name, "the top level")} and {hook.__name__}')
            out[name], owner[name] = a, hook.__name__
    return out


# ---------------------------------------------------------------- checker

def _scalar(e, name, case='missing'):
    if name not in e:
        raise ContractError(case, f'{name} is required')
    a = np.asarray(e[name])
    if a.ndim != 0:
        raise ContractError('mismatch', f'{name} must be a scalar, has shape {a.shape}')
    return a.item()


def _exact(e, prefix, want, why):
    """The entries under prefix are exactly want (empty: the part is absent)."""
    got = {n for n in e if n.startswith(prefix)}
    if want - got:
        raise ContractError('missing', f'{why}: {sorted(want - got)} missing')
    if got - want:
        raise ContractError('unknown-entry', f'{why}: {sorted(got - want)} not known')


def _measure(name, parts, finder):
    if name not in parts['measures']:
        raise ContractError('measure', f'measure {name!r} is not one the runtime runs ({sorted(parts["measures"])})')
    if name == 'f15' and finder != 'kick':
        raise ContractError('f15-finder', f'f15 comes with the kick finder, not {finder!r}')
    return name


def check(e, parts):
    """Raises ContractError for any entry set the runtime would refuse (section 4 (The model file contract v2))."""
    from tools.audio_analysis.lab.components import FITTED
    from tools.audio_analysis.lab.export import finder_kick, finder_snare, frames, kick2, measures, net, perc, stage, trees
    if 'contract' not in e or np.asarray(e['contract']).dtype != np.int32 or _scalar(e, 'contract') != CONTRACT:
        raise ContractError('contract', f'contract must be the i32 scalar {CONTRACT}')
    for name in sorted(TOP):
        if name not in e:
            raise ContractError('missing', f'{name} is required')
    for name in e:
        if name.startswith(RESERVED):
            raise ContractError('measure', f'{name}: res16 and glide3 do not run in the app yet')
        if name not in TOP and not name.startswith(PART_PREFIXES):
            raise ContractError('unknown-entry', f'{name} is not a contract entry')
    family = text(e['family'])
    if family not in FAMILIES:
        raise ContractError('family', f'unknown family {family!r}')
    for prefix in ('', 'base.', 'cand.', 'extra.', 'net.'):
        if f'{prefix}sample_rate' in e and _scalar(e, f'{prefix}sample_rate') != RATE:
            raise ContractError('grid', f'{prefix}sample_rate is not {RATE}')
        if f'{prefix}hop' in e and _scalar(e, f'{prefix}hop') != HOP:
            raise ContractError('grid', f'{prefix}hop is not {HOP}')
    if _scalar(e, 'rate') != RATE:
        raise ContractError('grid', f'rate is not {RATE}')

    finder = text(e['finder'])
    kind = finder.split(':')[0]
    if kind not in parts['finders'] or (kind == 'kick' and finder != 'kick') or \
            (kind == 'snare' and not re.fullmatch(r'snare:\d+(\.\d+)?', finder)):
        raise ContractError('finder', f'unknown finder {finder!r}')
    _exact(e, 'base.', finder_kick.NAMES if kind == 'kick' else frozenset(), 'kick finder')
    _exact(e, 'cand.', finder_snare.NAMES if kind == 'snare' else frozenset(), 'snare finder')
    if kind == 'snare' and _scalar(e, 'cand.rise_db') != finder_snare.rise_db(finder):
        raise ContractError('rise-db', f'cand.rise_db {e["cand.rise_db"]} is not the finder rule {finder!r}')

    models = text(e['models'])
    if models not in MODELS:
        raise ContractError('models', f'models {models!r} is not net, trees or net,trees')
    combine = text(e['combine'])
    if combine not in parts['combines']:
        raise ContractError('combine', f'unknown combine {combine!r}')
    from tools.audio_analysis.lab.config import COMBINES
    if COMBINES[combine] != MODELS[models]:
        raise ContractError('combine-models', f'combine {combine!r} does not match models {models!r}')
    for name in ('combine.clip', 'cutoff', 'refractory_s'):
        if not np.isfinite(_scalar(e, name)):
            raise ContractError('mismatch', f'{name} is not finite')

    used = []
    if 'trees' in MODELS[models]:
        tm = [_measure(m, parts, finder) for m in names_of(text(e.get('trees.measures', np.zeros(0, np.uint8))))]
        _exact(e, 'trees.', trees.NAMES, 'trees')
        if not tm:
            raise ContractError('missing', 'trees.measures names no measure')
        widths = [parts['measures'][m] for m in tm]
        if list(np.asarray(e['trees.widths'])) != widths:
            raise ContractError('width', f'trees.widths {list(e["trees.widths"])} are not {widths} for {tm}')
        if _scalar(e, 'trees.n_features') != sum(widths):
            raise ContractError('n-features', f'trees forest reads {e["trees.n_features"]} features, measures give {sum(widths)}')
        used += tm
    else:
        _exact(e, 'trees.', frozenset(), 'trees (models has none)')

    if 'net' in MODELS[models]:
        count = _scalar(e, 'net.count')
        if count < 1:
            raise ContractError('net-count', f'net.count {count} < 1')
        extra = names_of(text(e.get('net.extra', np.zeros(0, np.uint8))))
        for ch in extra:
            if ch not in parts['channels']:
                raise ContractError('channel', f'unknown frame channel {ch!r}')
        if 'net.channels' in e and len(extra) != _scalar(e, 'net.channels') - 2:
            raise ContractError('net-extra', f'net.extra {extra} does not fill net.channels {e["net.channels"]} - 2')
        weights = {n for n in e if re.fullmatch(r'net\.\d+\..+', n)}
        idx = {int(n.split('.')[1]) for n in weights}
        if idx - set(range(count)):
            raise ContractError('net-weights', f'weights for nets {sorted(idx - set(range(count)))} >= net.count {count}')
        for k in range(count):
            for w in net.WEIGHTS:
                if f'net.{k}.{w}' not in e:
                    raise ContractError('net-weights', f'net.{k}.{w} missing')
        for k in range(1, count):
            for w in net.WEIGHTS:
                if np.shape(e[f'net.{k}.{w}']) != np.shape(e[f'net.0.{w}']):
                    raise ContractError('net-shapes', f'net.{k}.{w} has another shape than net.0.{w}')
        _exact(e, 'net.', net.names(count), 'net')
        _exact(e, 'flat.', frames.NAMES if 'flat' in extra else frozenset(), 'flat channel')
        _exact(e, 'perc.', perc.NAMES if 'perc' in extra else frozenset(), 'perc channel')
    else:
        _exact(e, 'net.', frozenset(), 'net (models has none)')
        _exact(e, 'flat.', frozenset(), 'flat channel (no net)')
        _exact(e, 'perc.', frozenset(), 'perc channel (no net)')

    st = _scalar(e, 'stage')
    if st not in (0, 1) or np.asarray(e['stage']).dtype != np.int32:
        raise ContractError('mismatch', 'stage is the i32 scalar 0 or 1')
    if st:
        windows = np.asarray(e.get('stage.windows_s', np.zeros(0)))
        if windows.ndim != 1 or not 1 <= len(windows) <= parts['stage']['max_windows'] or not np.all(windows > 0):
            raise ContractError('windows', f'stage.windows_s {windows} is not 1..{parts["stage"]["max_windows"]} positive seconds')
        shape = [_measure(m, parts, finder) for m in names_of(text(e.get('stage.shape', np.zeros(0, np.uint8))))]
        if not shape:
            raise ContractError('missing', 'stage.shape names no measure')
        level = text(e.get('stage.level', np.zeros(0, np.uint8)))
        if level:
            m, _, col = level.partition(':')
            _measure(m, parts, finder)
            if not col.isdigit() or int(col) >= parts['measures'][m]:
                raise ContractError('level-column', f'stage.level {level!r}: column outside {m} (width {parts["measures"][m]})')
            used.append(m)
        gate = _scalar(e, 'stage.gate')
        cuts = {'stage.cut_detector', 'stage.cut_stage'} & set(e)
        if not gate >= 0 or (gate > 0) != (len(cuts) == 2) or (gate == 0 and cuts):
            raise ContractError('gate-cuts', f'gate {gate} with cuts {sorted(cuts)}: both cuts exactly when gate > 0')
        _exact(e, 'stage.', stage.names(gate), 'stage')
        want = stage.n_inputs(len(windows), bool(level))
        if _scalar(e, 'stage.n_features') != want:
            raise ContractError('n-features', f'stage forest reads {e["stage.n_features"]} inputs, {want} expected')
        used += shape
    else:
        _exact(e, 'stage.', frozenset(), 'stage (stage is 0)')

    _exact(e, 'extra.', measures.names_for(used), 'measure constants')
    _exact(e, 'kick2.', kick2.NAMES if 'kick2' in used else frozenset(), 'kick verdict')
    want_fit = frozenset().union(*(measures.FIT_NAMES[m] for m in set(used) if m in FITTED))
    have_fit = {n for n in e if n.startswith('fit.')}
    if want_fit - have_fit:
        raise ContractError('fitted', f'fitted measure params {sorted(want_fit - have_fit)} missing')
    _exact(e, 'fit.', want_fit, 'fitted measures')
