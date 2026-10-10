"""Training-only coverage extension; original evaluation records stay unchanged."""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import time

import numpy as np

from .kick_boosted_score import training_key
from .run_kick_fusion_trial import fit_without, training_labels
from .verify_kick_evening_passages import score_reviewed_core


def merge_training_cores(record, cores):
    """Use authoritative label semantics, rejecting overlaps instead of hiding them."""
    accepted = [p for p in cores if p['scoring_ready']]
    if len({p['id'] for p in cores}) != len(cores):
        raise ValueError('additional passage identities must be unique')
    for left, right in zip(sorted(accepted, key=lambda p:p['start_s']),
                           sorted(accepted, key=lambda p:p['start_s'])[1:]):
        if left['end_s'] > right['start_s']:
            raise ValueError('additional reviewed cores overlap')
    scored = [score_reviewed_core([], p) for p in accepted]
    source = dict(group='training_only_additional', passages=accepted)
    ref = dict(scores=dict(v5=scored))
    mask, labels, event_ids = training_labels(source, ref, record['candidates'],
        record['available'], record['sample_rate'], record['hop'],
        record['cache_metadata']['duration_s'])
    old_mask = record['training_mask']
    if np.any(old_mask & mask):
        raise ValueError('original and added selected training rows overlap')
    old_positive = old_mask & (record['labels'] == 1)
    offset = int(record['event_ids'][old_positive].max())+1 if old_positive.any() else 0
    shifted = event_ids.copy()
    shifted[mask & (labels == 1)] += offset
    result = dict(record, training_mask=old_mask | mask,
                  labels=record['labels'].copy(), event_ids=record['event_ids'].copy())
    result['labels'][mask] = labels[mask]
    result['event_ids'][mask] = shifted[mask]
    if any(not np.array_equal(result[n][old_mask], record[n][old_mask])
           for n in ('training_mask','labels','event_ids')):
        raise ValueError('original selected training rows changed')
    old_ids = set(record['event_ids'][old_positive].tolist())
    new_ids = set(shifted[mask & (labels == 1)].tolist())
    if old_ids & new_ids:
        raise ValueError('added positive event identity collides with original')
    events, local_offset = [], 0
    for p, score in zip(accepted, scored):
        for i, onset in enumerate(p['kick_times_s']):
            count = int(np.count_nonzero(mask & (labels == 1) & (event_ids == local_offset+i)))
            events.append(dict(passage=p['id'],time_s=onset,event_id=offset+local_offset+i,
                in_core=p['start_s'] <= onset < p['end_s'],selected_positive_rows=count))
        local_offset += len(p['kick_times_s'])
    rows = np.flatnonzero(mask)
    receipt = dict(track=record['track'],original_rows=int(old_mask.sum()),
        added_rows=len(rows),added_positive_rows=int(labels[mask].sum()),
        added_negative_rows=int(np.count_nonzero(mask & (labels == 0))),
        added_represented_events=len(new_ids),event_id_offset=offset,
        added_row_indices=rows.tolist(),added_event_ids=shifted[mask].tolist(),
        original_training_key=list(training_key(record)),merged_training_key=list(training_key(result)),
        original_selected_arrays_exact=True,selected_masks_disjoint=True,
        evaluation_objects_identical=result['source'] is record['source'] and result['ref'] is record['ref'],
        accepted_cores=[dict(id=p['id'],labels=s['labels'],excluded_regions=s['excluded_regions'],
            actual_review_bounds_s=s['actual_review_bounds_s'],
            recording_start_boundary=s['recording_start_boundary']) for p,s in zip(accepted,scored)],
        unscored_cores=[dict(id=p['id'],reason=p['reason']) for p in cores if not p['scoring_ready']],
        event_coverage=events)
    return result, receipt


def add_training_coverage(records, reviewed):
    if reviewed['status'] != 'lead_reviewed_visual_provisional':
        raise ValueError('additional labels require accepted lead review')
    by = {t['track']:t for t in reviewed['tracks']}
    if len(by) != len(reviewed['tracks']) or set(by)-{r['track'] for r in records}:
        raise ValueError('unknown or repeated additional review family')
    result, receipts = [], []
    for record in records:
        track = by.get(record['track'])
        if track and track['audio_sha256'] != record['cache_metadata']['signature']['audio_sha256']:
            raise ValueError('additional review and frozen feature source differ')
        merged, receipt = merge_training_cores(record, track['cores'] if track else [])
        result.append(merged); receipts.append(receipt)
    return result, receipts


class LinearFoldFitter:
    """Cache the unchanged original linear fitter by complete training input."""
    def __init__(self, cache):
        self.cache = Path(cache); self.cache.mkdir(parents=True,exist_ok=True)
        self.models = {}; self.fits = self.memory_hits = self.disk_hits = 0
        self.fit_cpu_s = 0.
        self.source_sha256 = {n:hashlib.sha256((Path(__file__).parent/n).read_bytes()).hexdigest()
            for n in ('kick_training_coverage.py','run_kick_fusion_trial.py','kick_boosted_score.py')}

    def fit(self, records, held_out):
        training = [r for r in records if r['track'] != held_out]
        if len({r['track'] for r in records}) != len(records) or not training:
            raise ValueError('unique training families required')
        keys = [list(training_key(r)) for r in training]
        signature = dict(training_input_keys=keys,source_sha256=self.source_sha256,l2=.01)
        key = hashlib.sha256(json.dumps(signature,sort_keys=True).encode()).hexdigest()
        path = self.cache/f'{key}.json'
        if key in self.models:
            self.memory_hits += 1
        elif path.exists():
            entry = json.loads(path.read_text())
            if entry['signature'] != signature or entry['model_sha256'] != hashlib.sha256(
                    json.dumps(entry['model'],sort_keys=True).encode()).hexdigest():
                raise ValueError('linear cache provenance mismatch')
            self.models[key] = entry['model']; self.disk_hits += 1
        else:
            started = time.process_time()
            model = fit_without(records,held_out)
            elapsed = time.process_time()-started
            model.pop('held_out')
            model.update(training_input_keys=keys,training_input_sha256=key,fit_cpu_s=elapsed)
            self.models[key] = model; self.fits += 1; self.fit_cpu_s += elapsed
            digest = hashlib.sha256(json.dumps(model,sort_keys=True).encode()).hexdigest()
            path.write_text(json.dumps(dict(signature=signature,model=model,model_sha256=digest))+'\n')
        model = copy.deepcopy(self.models[key]); model['held_out'] = held_out
        return model

    def statistics(self):
        return dict(fits=self.fits,memory_hits=self.memory_hits,disk_hits=self.disk_hits,
                    unique_models=len(self.models),fit_cpu_s=self.fit_cpu_s)
