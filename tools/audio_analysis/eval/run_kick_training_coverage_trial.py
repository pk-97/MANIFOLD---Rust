"""Three unchanged scorers with reviewed training-only coverage additions."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import kick_boosted_score as tree
from . import kick_interaction_score as interaction
from . import run_kick_fusion_trial as linear
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_fusion_calibration import threshold_scores
from .kick_subspace_score import _training_data
from .kick_training_coverage import LinearFoldFitter, add_training_coverage, merge_training_cores
from .run_kick_boosted_trial import score_diagnostics
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons
from .run_kick_shape_trial import add_cohort_results, append_reviews
from .verify_kick_evening_passages import aggregate, score_reviewed_core


CONFIGURATIONS = [dict(name='coverage_linear15',kind='linear',l2=.01),
    dict(name='coverage_tree15',kind='tree',depth=3,estimators=64),
    dict(name='coverage_interaction15',kind='interaction',l2=.01)]


def replay_frozen(records, variant, predict):
    """Zero additions reproduce complete frozen emissions and scores, with no fit."""
    checked = 0
    for original in records:
        record, _ = merge_training_cores(original,[])
        if tree.training_key(record) != tree.training_key(original):
            raise ValueError('zero additions changed training input')
        row = next(r for r in variant['tracks'] if r['track']==record['track'])
        values = predict(row['model'],record['features'])
        for key,cutoff in (('fixed_05',.5),('calibrated',row['calibration']['threshold']),
                           ('refined',row['refinement']['threshold'])):
            fires, scores = threshold_scores(record,values,cutoff)
            if fires != row['kick_hops'][key] or scores != row['scores'][key]:
                raise ValueError(f'zero-addition replay differs: {row["track"]}/{key}')
            checked += 1
    return dict(variant=variant['variant'],tracks=len(records),exact_event_and_score_cases=checked,
                zero_addition_training_inputs_identical=True,no_refits=True)


def audit_models(variant, records, originals):
    """Verify every complete-family exclusion, keys, weights and normalisation."""
    names = {r['track'] for r in records}; normalisations = {}; models = 0
    for row in variant['tracks']:
        outer = row['track']; old = next(r for r in originals if r['track']==outer)
        if row['scores']['baseline'] != old['ref']['scores']['v5']:
            raise ValueError('original evaluation reference changed')
        for field in ('calibration','refinement'):
            if set(row[field]['threshold_selection_tracks']) != names-{outer}:
                raise ValueError('cutoff selection family leakage')
        for model,excluded in [(row['model'],{outer})]+[(f['model'],{outer,f['validation_track']})
                for f in row['calibration']['inner_folds']]:
            training = [r for r in records if r['track'] not in excluded]
            if set(model['training_tracks']) != names-excluded:
                raise ValueError('model family leakage')
            if model['training_input_keys'] != [list(tree.training_key(r)) for r in training]:
                raise ValueError('training cache inputs differ')
            key = tuple(model['training_tracks'])
            if key not in normalisations:
                _,x,y,w = _training_data(training,'none')
                mean = np.sum(x*w[:,None],axis=0)
                scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:,None],axis=0)),1e-3)
                if not (abs(w.sum()-1)<1e-12 and abs(w[y==1].sum()-.5)<1e-12):
                    raise ValueError('class or total weight mass differs')
                normalisations[key] = mean,scale
            mean,scale = normalisations[key]
            if not np.array_equal(model['mean'],mean) or not np.array_equal(model['scale'],scale):
                raise ValueError('training-fold normalisation differs')
            models += 1
    return dict(model_records=models,unique_normalisations=len(normalisations),
        all_outer_inner_family_exclusions_exact=True,training_input_keys_exact=True,
        weighted_normalisations_exact=True,original_reference_scores_exact=True)


def add_diagnostics(variant, records, reference, same_scorer, predict):
    for row,record in zip(variant['tracks'],records):
        for name,frozen in (('linear15',reference),('same_scorer',same_scorer)):
            old = next(r for r in frozen['tracks'] if r['track']==row['track'])
            comparison = comparisons(old['scores']['refined'],row['scores']['refined'])
            row['comparison_to_'+name] = comparison
            row[name+'_delta'] = dict(lost_labels70=sum(len(p['metrics']['70']['lost_labels_s']) for p in comparison),
                recovered_labels70=sum(len(p['metrics']['70']['recovered_labels_s']) for p in comparison))
        row['score_distribution'] = score_diagnostics(record,predict(row['model'],record['features']),row)
        horizon = (record['available']-record['candidates'])*record['hop']/record['sample_rate']*1000
        delays = [p['delay_ms'] for passage in row['scores']['refined']
                  for p in passage['association_early_35_late_200_ms']['pairs']]
        row['timing'] = dict(candidate_observation_min_ms=float(horizon.min()),
            candidate_observation_max_ms=float(horizon.max()),emitted_at_completed_availability_hop=True,
            wide_associated_later_than70ms=sum(d>70 for d in delays))
    cores = [dict(track=r['track'],passage=p['id'],extra70=p['accuracy_by_tolerance_ms']['70']['extra'])
             for r in variant['tracks'] for p in r['scores']['refined'] if p['labels']==0]
    if len(cores)!=9: raise ValueError('expected nine original negative cores')
    counts = variant['totals']['refined']['tolerance_ms']['70']
    variant['kick_free_cores'] = cores
    variant['acceptance'] = dict(count_target_met=(counts['matched']>=223 and counts['extra']<=70)
        or (counts['matched']>=250 and counts['extra']<=89),
        nine_kick_free_cores_zero=all(c['extra70']==0 for c in cores),
        at_most_one_lost_label_per_track=all(r['linear15_delta']['lost_labels70']<=1 for r in variant['tracks']))
    variant['acceptance']['intermediate_target_met'] = all(variant['acceptance'].values())


def score_additional(variant, reviewed):
    rows, unscored = [], []
    for track in reviewed['tracks']:
        row = next(r for r in variant['tracks'] if r['track']==track['track'])
        if track['track'] in row['model']['training_tracks']:
            raise ValueError('additional review family leaked into fit')
        predictions = [(i+1)*row['hop']/row['sample_rate'] for i in row['kick_hops']['refined']]
        for core in track['cores']:
            if not core['scoring_ready']:
                unscored.append(dict(track=track['track'],passage=core['id'],reason=core['reason']));continue
            rows.append(dict(track=track['track'],passage=core['id'],**score_reviewed_core(predictions,core)))
    return dict(variant=variant['variant'],totals=aggregate(rows),passages=rows,unscored=unscored,
        per_track={name:aggregate([r for r in rows if r['track']==name]) for name in sorted({r['track'] for r in rows})},
        negative_cores={r['passage']:r['accuracy_by_tolerance_ms']['70']['extra'] for r in rows if r['labels']==0})


def run(audio_root, cache, rule_path, out):
    for name in ('OPENBLAS_NUM_THREADS','OMP_NUM_THREADS','VECLIB_MAXIMUM_THREADS'):
        if os.environ.get(name)!='1': raise ValueError('one CPU thread required')
    rule = json.loads(rule_path.read_text())
    if rule['configurations'] != CONFIGURATIONS: raise ValueError('predeclaration differs')
    inputs = {}
    for name,item in rule['inputs'].items():
        path=Path(item['path'])
        if sha(path)!=item['sha256']: raise ValueError(f'frozen input changed: {name}')
        inputs[name]=json.loads(path.read_text())
    for report_name in ('previous','tree','interaction'):
        for name,digest in inputs[report_name]['source_sha256'].items():
            if sha(ROOT/'tools/audio_analysis/eval'/name)!=digest:
                raise ValueError(f'frozen upstream changed: {name}')
    def forbidden(*a,**kw): raise RuntimeError('new audio or extraction forbidden')
    with patch.object(cached,'read_audio',side_effect=forbidden),patch.object(cached,'fusion_features',side_effect=forbidden):
        records = runner.load_records(audio_root,inputs['baseline'],cache)
    records,expanded_coverage = append_reviews(records,inputs['expanded'])
    originals = [dict(r,features=r['features'][:,:15]) for r in records]
    evaluation_hash = hashlib.sha256(json.dumps([(r['source'],r['ref']) for r in originals],sort_keys=True).encode()).hexdigest()
    frozen = [next(v for v in inputs[name]['variants'] if v['variant']==variant)
        for name,variant in (('previous','linear_15'),('tree','boosted_depth3'),('interaction','interaction_0.01'))]
    predictors = [linear.predict_score,tree.predict_score,interaction.predict_score]
    replays = [replay_frozen(originals,v,p) for v,p in zip(frozen,predictors)]
    records,coverage = add_training_coverage(originals,inputs['additional'])
    if sum(c['labels'] for r in coverage for c in r['accepted_cores'])!=73:
        raise ValueError('additional73 labels changed')
    if sum(len(r['accepted_cores']) for r in coverage)!=6:
        raise ValueError('expected six accepted additional cores')
    if next(r for r in coverage if r['track']=='late_night')['added_rows']:
        raise ValueError('unknown Late Night truth included')
    if hashlib.sha256(json.dumps([(r['source'],r['ref']) for r in records],sort_keys=True).encode()).hexdigest()!=evaluation_hash:
        raise ValueError('original scoring/calibration objects changed')
    out.parent.mkdir(parents=True,exist_ok=True)
    files = ('kick_training_coverage.py','run_kick_training_coverage_trial.py','test_kick_training_coverage.py',
        'run_kick_fusion_trial.py','kick_boosted_score.py','kick_interaction_score.py','kick_subspace_score.py',
        'kick_fusion_calibration.py','kick_fusion_fine_calibration.py','verify_kick_evening_passages.py',
        'master_kick_comparison.py','run_kick_shape_trial.py','run_kick_trajectory_trial.py')
    report = dict(hypothesis=rule,rule_sha256=sha(rule_path),complete=False,variants=[],
        zero_addition_controls=replays,training_coverage=coverage,expanded_coverage=expanded_coverage,
        evaluation_source_ref_sha256=evaluation_hash,original_evaluation_objects_exact=True,
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in files},
        limitations=rule['additional_review_status']+' No reserved or live evaluation. Provisional labels unchanged.')
    out.write_text(json.dumps(report,indent=2)+'\n')
    linear_fitter = LinearFoldFitter(out.parent/'models_linear')
    tree_fitter = tree.FoldFitter(out.parent/'models_tree')
    interaction_fitter = interaction.FoldFitter(out.parent/'models_interaction')
    fits=[linear_fitter.fit,lambda rows,held:tree_fitter.fit(rows,held,3),
          lambda rows,held:interaction_fitter.fit(rows,held,.01)]
    for config,fitter,fit,predict,control in zip(CONFIGURATIONS,
            (linear_fitter,tree_fitter,interaction_fitter),fits,predictors,frozen):
        variant = runner.run_variant(records,config['name'],fit,predict)
        add_cohort_results(variant,originals)
        if variant['totals']['refined']['labels']!=381 or variant['cohorts']['expanded']['refined']['labels']!=207:
            raise ValueError('original381 evaluation changed')
        add_diagnostics(variant,records,frozen[0],control,predict)
        variant['fold_audit'] = audit_models(variant,records,originals)
        variant['fit_cache'] = fitter.statistics()
        report['variants'].append(variant)
        report['complete'] = len(report['variants'])==len(CONFIGURATIONS)
        out.write_text(json.dumps(report,indent=2)+'\n')
        print('TOTAL',config['name'],variant['totals']['refined']['tolerance_ms']['70'],
              variant['acceptance'],fitter.statistics(),flush=True)
    # Freeze original381 results before any scoring of the newly reused73 labels.
    original_results_sha = sha(out)
    additional = dict(original381_trial_sha256=original_results_sha,
        labels_sha256=rule['inputs']['additional']['sha256'],
        disclosure=rule['additional_review_status'],
        variants=[score_additional(v,inputs['additional']) for v in report['variants']])
    (out.parent/'additional73.json').write_text(json.dumps(additional,indent=2)+'\n')
    print('ADDITIONAL73',[(v['variant'],v['totals']['accuracy_by_tolerance_ms']['70'],v['negative_cores'])
        for v in additional['variants']],flush=True)
    return report


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root','cache','rule','out'): parser.add_argument('--'+name,type=Path,required=True)
    a=parser.parse_args();run(a.audio_root,a.cache,a.rule,a.out)
