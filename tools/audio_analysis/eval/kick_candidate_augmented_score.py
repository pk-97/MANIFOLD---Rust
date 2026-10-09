"""H7 natural-candidate stem augmentation with explicit empty-condition budgets."""
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
from .kick_fusion_bandwise import FEATURE_NAMES, fusion_features
from .kick_controlled_mixtures import construct_conditions, SR
from .kick_subspace_score import _training_data

MASSES = (.1, .25, .5)
FAMILIES = ('late_night', 'midnight_patience', 'miracle', 'heavy_on_mind')
CONDITIONS = ((1, 0.), (1, 1.), (1, 2.), (0, 1.), (0, 2.))


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def eligible_candidates(available, hop, sample_rate, anchor_sample):
    """Use exact local source samples: both endpoints include actual emissions."""
    end = (np.asarray(available)+1)*hop
    return np.flatnonzero((end >= anchor_sample) & (end <= anchor_sample+.070*sample_rate))


def prepare_augmentation(frozen_path, d1_path, out):
    frozen, d1 = (json.loads(Path(p).read_text()) for p in (frozen_path, d1_path))
    if sha(frozen_path) != d1['frozen_contexts_sha256']:
        raise ValueError('D1 frozen context identity changed')
    for name, digest in d1['dependency_sha256'].items():
        if sha(Path(__file__).parent/name) != digest:
            raise ValueError('D1 feature dependency changed')
    contexts = []
    parity_rows = missing_old_rows = 0
    for c in frozen['contexts']:
        if c['track'] not in FAMILIES or sha(c['cache']) != c['cache_sha256']:
            raise ValueError('unexpected family or changed local source cache')
        with np.load(c['cache']) as data:
            common, signals = construct_conditions(data['kick'], data['accompaniment'])
        if common != c['common_gain']:
            raise ValueError('frozen common gain changed')
        old = next(x for x in d1['contexts'] if x['id'] == c['id'])
        rows = []
        for label, gain in CONDITIONS:
            candidates, available, features, hop = fusion_features(signals[gain, bool(label)], SR)
            starts = c['master_origin_s']+(candidates+1)*hop/SR
            emissions = c['master_origin_s']+(available+1)*hop/SR
            previous = next(x for x in old['conditions'] if x['gain'] == gain and x['kick_present'] == bool(label))
            np.testing.assert_array_equal(starts, previous['all_candidate_starts_master_s'])
            np.testing.assert_array_equal(emissions, previous['all_candidate_emissions_master_s'])
            stored_hops = set()
            for p in previous['nearby_candidates']:
                index = np.flatnonzero(starts == p['candidate_master_s'])
                if len(index) != 1:
                    raise ValueError('D1 natural candidate not found')
                np.testing.assert_array_equal(features[index[0]], [p['features'][name] for name in FEATURE_NAMES])
                stored_hops.add(int(candidates[index[0]])); parity_rows += 1
            selected = eligible_candidates(available, hop, SR, c['source_anchor_sample'])
            missing_old_rows += sum(int(candidates[j]) not in stored_hops for j in selected)
            rows.append(dict(label=label, gain=gain, candidate_hops=candidates[selected].tolist(),
                available_hops=available[selected].tolist(), candidate_master_s=starts[selected].tolist(),
                emission_master_s=emissions[selected].tolist(), features=features[selected].tolist(),
                candidate_count=len(selected), source_anchor_sample=c['source_anchor_sample'],
                full_condition_candidate_count=len(candidates)))
        contexts.append(dict(id=c['id'], track=c['track'], source_anchor_s=c['source_anchor_master_s'],
            common_gain=common, source_cache=c['cache'], source_cache_sha256=c['cache_sha256'],
            sample_rate=SR, hop=hop, master_origin_s=c['master_origin_s'], conditions=rows))
    report = dict(feature_names=list(FEATURE_NAMES), frozen_contexts_sha256=sha(frozen_path),
        d1_results_sha256=sha(d1_path), source_sha256=sha(__file__), contexts=contexts,
        verification=dict(overlapping_d1_natural_feature_rows_exact=parity_rows,
            selected_rows_missing_from_old_nearby_cache=missing_old_rows,
            all_natural_candidate_and_emission_coordinates_exact=True),
        method='Recomputed only80 cached2second D1 conditions with unchanged runtime extractor. '
            'Select actual natural candidate emission in inclusive source-anchor0..70ms; no score selection.')
    Path(out).write_text(json.dumps(report, indent=2)+'\n')
    # JSON floats roundtrip to the same feature values used by the runtime extractor.
    if json.loads(Path(out).read_text()) != report:
        raise ValueError('candidate feature JSON roundtrip failed')
    return report


def load_augmentation(path):
    report = json.loads(Path(path).read_text())
    if report['feature_names'] != list(FEATURE_NAMES):
        raise ValueError('frozen fifteen features required')
    groups = {}
    for c in report['contexts']:
        if c['track'] not in FAMILIES:
            raise ValueError('unexpected source family')
        group = groups.setdefault(c['track'], dict(features=[], labels=[], context_ids=[], gains=[], condition_ids=[], conditions=[]))
        if {(r['label'], r['gain']) for r in c['conditions']} != set(CONDITIONS) or len(c['conditions']) != 5:
            raise ValueError('exactly five declared conditions required')
        for row in c['conditions']:
            cid = f"{c['id']}:{row['label']}:{row['gain']:g}"
            count = len(row['features'])
            if count != row['candidate_count']:
                raise ValueError('candidate row count differs')
            group['conditions'].append(dict(id=cid, context=c['id'], label=row['label'], gain=row['gain'], count=count))
            group['features'].extend(row['features']); group['labels'].extend([row['label']]*count)
            group['context_ids'].extend([c['id']]*count); group['gains'].extend([row['gain']]*count)
            group['condition_ids'].extend([cid]*count)
    if set(groups) != set(FAMILIES):
        raise ValueError('four source families required')
    for group in groups.values():
        if len({c['context'] for c in group['conditions']}) != 4:
            raise ValueError('four frozen contexts required per family')
        for key in ('features', 'labels', 'context_ids', 'gains', 'condition_ids'):
            group[key] = np.asarray(group[key])
        group['features'] = group['features'].reshape(-1, len(FEATURE_NAMES))
    return groups


def _augmentation_key(family, group):
    digest = hashlib.sha256(json.dumps(group['conditions'], sort_keys=True).encode())
    for name in ('features', 'labels', 'context_ids', 'gains', 'condition_ids'):
        values = np.asarray(group[name])
        digest.update(str((name, values.shape, values.dtype.str)).encode()); digest.update(values.tobytes())
    return family, digest.hexdigest()


def augmented_training_data(records, held_out, augmentation, mass):
    if isinstance(mass, (bool, np.bool_)) or mass not in (0.,)+MASSES:
        raise ValueError('only predeclared masses or zero control allowed')
    training, original_x, original_y, original_w = _training_data(records, held_out)
    mean = np.sum(original_x*original_w[:, None], axis=0)
    scale = np.maximum(np.sqrt(np.sum((original_x-mean)**2*original_w[:, None], axis=0)), 1e-3)
    natural_w = original_w.copy()
    arrays, labels, weights = [original_x], [original_y], [natural_w]
    original_families = np.concatenate([np.repeat(r['track'], np.count_nonzero(r['training_mask'])) for r in training])
    families, kinds, contexts = [original_families], [np.repeat('natural',len(original_y))], [np.repeat('',len(original_y))]
    used, budgets, offset = [], [], 0
    for record in training:
        count = np.count_nonzero(record['training_mask']); family = record['track']
        if mass and family in augmentation:
            g = augmentation[family]; x=np.asarray(g['features']); y=np.asarray(g['labels']); ids=np.asarray(g['condition_ids'])
            if x.ndim != 2 or x.shape[1] != original_x.shape[1] or y.shape != (len(x),) or ids.shape != y.shape or not np.all(np.isfinite(x)) or not np.all(np.isin(y,(0,1))):
                raise ValueError('invalid candidate augmentation arrays')
            catalog = g['conditions']; expected={(c['context'],label,gain) for c in catalog for label,gain in CONDITIONS}
            if len(catalog)!=len(expected) or {(c['context'],c['label'],c['gain']) for c in catalog}!=expected or len({c['id'] for c in catalog})!=len(catalog):
                raise ValueError('complete unique context/class/gain catalog required, including empty conditions')
            if set(ids)-{c['id'] for c in catalog}:
                raise ValueError('orphan candidate condition')
            w=np.zeros(len(y)); class_rows=[]
            for label in (0,1):
                slots=[c for c in catalog if c['label']==label]; nominal=mass/len(training)/2/len(slots); empty=[]
                for condition in slots:
                    selected=ids==condition['id']; n=np.count_nonzero(selected)
                    if n!=condition['count'] or np.any(y[selected]!=label) or np.any(np.asarray(g['context_ids'])[selected]!=condition['context']) or np.any(np.asarray(g['gains'])[selected]!=condition['gain']):
                        raise ValueError('candidate condition metadata differs')
                    if n:w[selected]=nominal/n
                    else:empty.append(condition['id'])
                allocated=float(w[y==label].sum()); requested=mass/len(training)/2
                natural=np.arange(offset,offset+count)[original_y[offset:offset+count]==label]
                natural_w[natural]*=1-allocated/(.5/len(training))
                class_rows.append(dict(label=label,requested=requested,allocated=allocated,returned=requested-allocated,empty_conditions=empty))
            arrays.append(x);labels.append(y);weights.append(w);families.append(np.repeat(family,len(y)))
            kinds.append(np.repeat('augmented',len(y)));contexts.append(np.asarray(g['context_ids']));used.append(family)
            budgets.append(dict(track=family,requested_mass=mass/len(training),effective_mass=float(w.sum()),classes=class_rows))
        offset+=count
    return dict(training=training,x=np.vstack(arrays),y=np.concatenate(labels),w=np.concatenate(weights),mean=mean,scale=scale,
        families=np.concatenate(families),kinds=np.concatenate(kinds),context_ids=np.concatenate(contexts),augmentation_families=used,augmentation_budgets=budgets)


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
            for name in ('kick_candidate_augmented_score.py', 'kick_stem_augmented_score.py', 'kick_boosted_score.py', 'kick_subspace_score.py')}

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
                augmentation_budgets=data['augmentation_budgets'],
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
