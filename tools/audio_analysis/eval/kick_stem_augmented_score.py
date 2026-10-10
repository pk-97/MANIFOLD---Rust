"""Paired stem augmentation with exact original normalisation and fold exclusions."""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import time

import numpy as np
import sklearn
from sklearn.ensemble import GradientBoostingClassifier

from .kick_boosted_score import _standardise, export_model, parameters, predict_score, training_key
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_subspace_score import _training_data

MASSES = (.1, .25, .5)
FAMILIES = ('late_night', 'midnight_patience', 'miracle', 'heavy_on_mind')


def load_augmentation(path):
    report = json.loads(Path(path).read_text())
    if report['feature_names'] != list(FEATURE_NAMES):
        raise ValueError('augmentation must use the frozen fifteen features')
    groups = {}
    for context in report['contexts']:
        family = context['track']
        if family not in FAMILIES:
            raise ValueError('augmentation contains an unexpected source family')
        group = groups.setdefault(family, dict(features=[], labels=[], context_ids=[], gains=[]))
        chosen = [r for r in context['conditions'] if r['kick_present'] or r['gain'] in (1, 2)]
        identities = {(bool(r['kick_present']), float(r['gain'])) for r in chosen}
        if len(chosen) != 5 or identities != {(True, 0.), (True, 1.), (True, 2.), (False, 1.), (False, 2.)}:
            raise ValueError('each context requires exactly three present and two removed rows')
        for row in chosen:
            group['features'].append([row['fixed_anchor_features'][name] for name in FEATURE_NAMES])
            group['labels'].append(int(row['kick_present']))
            group['context_ids'].append(context['id'])
            group['gains'].append(row['gain'])
    if set(groups) != set(FAMILIES):
        raise ValueError('expected all four declared source families')
    for group in groups.values():
        for key in group:
            group[key] = np.asarray(group[key])
        if len(np.unique(group['context_ids'])) != 4 or len(group['labels']) != 20:
            raise ValueError('expected four frozen contexts per source family')
    return groups


def _augmentation_key(family, group):
    digest = hashlib.sha256()
    for name in ('features', 'labels', 'context_ids', 'gains'):
        values = np.asarray(group[name])
        digest.update(str((name, values.shape, values.dtype.str)).encode())
        digest.update(values.tobytes())
    return family, digest.hexdigest()


def augmented_training_data(records, held_out, augmentation, mass):
    """Split available families' existing mass, preserving song/class/event weights."""
    if isinstance(mass, (bool, np.bool_)) or mass not in (0.,) + MASSES:
        raise ValueError('only the frozen augmentation masses (or zero control) are allowed')
    training, original_x, original_y, original_w = _training_data(records, held_out)
    mean = np.sum(original_x*original_w[:, None], axis=0)
    scale = np.maximum(np.sqrt(np.sum((original_x-mean)**2*original_w[:, None], axis=0)), 1e-3)
    natural_w = original_w.copy()
    arrays, labels, weights = [original_x], [original_y], [natural_w]
    original_families = np.concatenate([np.repeat(r['track'], np.count_nonzero(r['training_mask'])) for r in training])
    families, kinds = [original_families], [np.repeat('natural', len(original_y))]
    contexts = [np.repeat('', len(original_y))]
    used, offset = [], 0
    for record in training:
        count = np.count_nonzero(record['training_mask'])
        family = record['track']
        if mass and family in augmentation:
            group = augmentation[family]
            x, y, ids = np.asarray(group['features']), np.asarray(group['labels']), np.asarray(group['context_ids'])
            if x.ndim != 2 or x.shape[1] != original_x.shape[1] or y.shape != (len(x),) or ids.shape != y.shape:
                raise ValueError('augmentation shapes must match original features')
            if not np.all(np.isfinite(x)) or not np.all(np.isin(y, (0, 1))):
                raise ValueError('augmentation features must be finite with binary classes')
            natural_w[offset:offset+count] *= 1-mass
            w = np.zeros(len(y))
            for label, expected_count in ((0, 2), (1, 3)):
                class_ids = np.unique(ids[y == label])
                if len(class_ids) != len(np.unique(ids)) or not len(class_ids):
                    raise ValueError('both augmentation classes required in every context')
                for context in class_ids:
                    selected = (y == label) & (ids == context)
                    if np.count_nonzero(selected) != expected_count:
                        raise ValueError('wrong number of paired rows per context and class')
                    w[selected] = mass / len(training) / 2 / len(class_ids) / expected_count
            arrays.append(x); labels.append(y); weights.append(w)
            families.append(np.repeat(family, len(y))); kinds.append(np.repeat('augmented', len(y)))
            contexts.append(ids)
            used.append(family)
        offset += count
    return dict(training=training, x=np.vstack(arrays), y=np.concatenate(labels), w=np.concatenate(weights),
        mean=mean, scale=scale, families=np.concatenate(families), kinds=np.concatenate(kinds),
        context_ids=np.concatenate(contexts), augmentation_families=used)


class FoldFitter:
    def __init__(self, augmentation, cache=None):
        self.augmentation = augmentation
        self.cache = Path(cache) if cache is not None else None
        if self.cache is not None:
            self.cache.mkdir(parents=True, exist_ok=True)
        self.models = {}
        self.fits = self.memory_hits = self.disk_hits = 0
        self.fit_cpu_s = 0.
        self.source_sha256 = {name: hashlib.sha256((Path(__file__).parent/name).read_bytes()).hexdigest()
            for name in ('kick_stem_augmented_score.py', 'kick_boosted_score.py', 'kick_subspace_score.py')}

    def fit(self, records, held_out, mass):
        if isinstance(mass, (bool, np.bool_)) or mass not in (0.,) + MASSES:
            raise ValueError('augmentation mass is not predeclared')
        names = [r['track'] for r in records]
        if len(set(names)) != len(names):
            raise ValueError('track identities must be unique')
        training = [r for r in records if r['track'] != held_out]
        if not training:
            raise ValueError('no training songs')
        keys = [training_key(r) for r in training]
        augmentation_keys = [_augmentation_key(r['track'], self.augmentation[r['track']]) for r in training
                             if mass and r['track'] in self.augmentation]
        signature = dict(training_input_keys=keys, augmentation_input_keys=augmentation_keys,
            augmentation_mass=mass, parameters=parameters(3), sklearn_version=sklearn.__version__,
            source_sha256=self.source_sha256)
        signature = json.loads(json.dumps(signature))
        key = hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()
        path = self.cache/f'{key}.json' if self.cache is not None else None
        if key in self.models:
            self.memory_hits += 1
        elif path is not None and path.exists():
            stored = json.loads(path.read_text())
            if stored['signature'] != signature:
                raise ValueError('augmented fit cache provenance mismatch')
            digest = hashlib.sha256(json.dumps(stored['model'], sort_keys=True).encode()).hexdigest()
            if stored['model_sha256'] != digest:
                raise ValueError('augmented fit cache checksum mismatch')
            self.models[key] = stored['model']; self.disk_hits += 1
        else:
            data = augmented_training_data(records, held_out, self.augmentation, mass)
            x, y, w = data['x'], data['y'], data['w']
            z = _standardise(data['mean'], data['scale'], x)
            started = time.process_time()
            classifier = GradientBoostingClassifier(**parameters(3)).fit(z, y, sample_weight=w)
            cpu = time.process_time()-started
            model = export_model(classifier, data['mean'], data['scale'], [r['track'] for r in training])
            model = json.loads(json.dumps(model))
            error = float(np.max(np.abs(predict_score(model, x)-classifier.predict_proba(z)[:, 1])))
            if error > 1e-12:
                raise ValueError('JSON inference differs from sklearn')
            by_family = []
            for record in training:
                selected = data['families'] == record['track']
                by_family.append(dict(track=record['track'], total=float(w[selected].sum()),
                    positive=float(w[selected & (y == 1)].sum()), negative=float(w[selected & (y == 0)].sum()),
                    natural=float(w[selected & (data['kinds'] == 'natural')].sum()),
                    augmented=float(w[selected & (data['kinds'] == 'augmented')].sum())))
            model.update(augmentation_mass=mass, augmentation_families=data['augmentation_families'],
                normalisation='original training rows and original weights only', training_weight_by_family=by_family,
                training_samples=len(y), training_weight_mass=float(w.sum()),
                positive_weight_mass=float(w[y == 1].sum()), negative_weight_mass=float(w[y == 0].sum()),
                training_input_keys=keys, augmentation_input_keys=augmentation_keys, training_input_sha256=key,
                parameters=parameters(3), export_max_abs_error=error, fit_cpu_s=cpu, sklearn_version=sklearn.__version__)
            model = json.loads(json.dumps(model)); self.models[key] = model
            self.fits += 1; self.fit_cpu_s += cpu
            if path is not None:
                digest = hashlib.sha256(json.dumps(model, sort_keys=True).encode()).hexdigest()
                path.write_text(json.dumps(dict(signature=signature, model=model, model_sha256=digest), separators=(',', ':'))+'\n')
        result = copy.deepcopy(self.models[key]); result['held_out'] = held_out
        return result

    def statistics(self):
        return dict(unique_models=len(self.models), fits=self.fits, memory_hits=self.memory_hits,
                    disk_hits=self.disk_hits, fit_cpu_s=self.fit_cpu_s)
