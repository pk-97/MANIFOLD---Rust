"""H15: second coverage extension on unchanged nested scorers and evaluation."""
from __future__ import annotations

import argparse, hashlib, json, os
from pathlib import Path
from unittest.mock import patch

from . import kick_trajectory_features as cached
from . import kick_boosted_score as tree
from . import kick_interaction_score as interaction
from . import run_kick_fusion_trial as linear
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_training_coverage import LinearFoldFitter, add_training_coverage
from .run_kick_training_coverage_trial import replay_frozen, audit_models, add_diagnostics, score_additional
from .run_kick_dsp_experiments import ROOT
from .run_kick_shape_trial import append_reviews, add_cohort_results

CONFIGURATIONS = [dict(name='extended_coverage_linear15',kind='linear',l2=.01),
    dict(name='extended_coverage_tree15',kind='tree',depth=3,estimators=64),
    dict(name='extended_coverage_interaction15',kind='interaction',l2=.01)]


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
    for report_name in ('previous','tree','interaction','covered'):
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
    covered = inputs['covered']['variants']
    if [v['variant'] for v in covered] != ['coverage_linear15','coverage_tree15','coverage_interaction15']:
        raise ValueError('H10 reference order differs')
    replays.extend(replay_frozen(records,v,p) for v,p in zip(covered,predictors))
    records,extension_coverage = add_training_coverage(records,inputs['extension'])
    if sum(c['labels'] for r in extension_coverage for c in r['accepted_cores']) != 32:
        raise ValueError('accepted D2 scored-label count changed')
    if sum(len(r['accepted_cores']) for r in extension_coverage) != 8:
        raise ValueError('D2 must retain all eight frozen cores')
    combined = json.loads(json.dumps(inputs['additional']))
    extension_by = {t['track']:t for t in inputs['extension']['tracks']}
    for t in combined['tracks']:
        t['cores'].extend(extension_by[t['track']]['cores'])
    if hashlib.sha256(json.dumps([(r['source'],r['ref']) for r in records],sort_keys=True).encode()).hexdigest()!=evaluation_hash:
        raise ValueError('original scoring/calibration objects changed')
    out.parent.mkdir(parents=True,exist_ok=True)
    files = ('kick_training_coverage.py','run_kick_training_coverage_trial.py','run_kick_extended_coverage_trial.py','test_kick_training_coverage.py',
        'run_kick_fusion_trial.py','kick_boosted_score.py','kick_interaction_score.py','kick_subspace_score.py',
        'kick_fusion_calibration.py','kick_fusion_fine_calibration.py','verify_kick_evening_passages.py',
        'master_kick_comparison.py','run_kick_shape_trial.py','run_kick_trajectory_trial.py')
    report = dict(hypothesis=rule,rule_sha256=sha(rule_path),complete=False,variants=[],
        zero_addition_controls=replays,training_coverage=coverage,extension_coverage=extension_coverage,expanded_coverage=expanded_coverage,
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
            (linear_fitter,tree_fitter,interaction_fitter),fits,predictors,covered):
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
        labels_sha256={k:rule['inputs'][k]['sha256'] for k in ('additional','extension')},
        disclosure=rule['additional_review_status'],
        variants=[score_additional(v,combined) for v in report['variants']])
    (out.parent/'additional105.json').write_text(json.dumps(additional,indent=2)+'\n')
    print('ADDITIONAL105',[(v['variant'],v['totals']['accuracy_by_tolerance_ms']['70'],v['negative_cores'])
        for v in additional['variants']],flush=True)
    return report


if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root','cache','rule','out'): parser.add_argument('--'+name,type=Path,required=True)
    a=parser.parse_args();run(a.audio_root,a.cache,a.rule,a.out)
