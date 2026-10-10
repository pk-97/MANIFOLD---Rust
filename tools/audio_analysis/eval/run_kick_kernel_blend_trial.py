"""H17 fixed no-fit blends, compact references and a120-CPU-second evaluation cap."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from unittest.mock import patch

from . import kick_trajectory_features as cached,run_kick_trajectory_trial as runner
from . import kick_kernel_score as kernel,run_kick_fusion_trial as linear
from .kick_kernel_blend import KERNEL_WEIGHTS,FrozenFitter,PredictionCache,frozen_rule,sha
from .kick_training_coverage import add_training_coverage
from .kick_fusion_calibration import threshold_scores
from .run_kick_dsp_experiments import ROOT
from .run_kick_shape_trial import append_reviews,add_cohort_results
from .run_kick_training_coverage_trial import add_diagnostics,audit_models,replay_frozen,score_additional
from .run_kick_fusion_calibration import comparisons


def audit_components(variant):
    count=0
    for row in variant['tracks']:
        for model,excluded in [(row['model'],{row['track']})]+[(f['model'],{row['track'],f['validation_track']}) for f in row['calibration']['inner_folds']]:
            for component in (model['linear'],model['kernel_reference']):
                if excluded.intersection(component['training_tracks']):raise ValueError('excluded family leaked')
                for key in ('training_tracks','training_input_keys','mean','scale'):
                    if component[key]!=model[key]:raise ValueError('component provenance differs')
            count+=1
    return dict(blend_models_checked=count,component_models_checked=count*2,all_exclusions_keys_normalisations_exact=True)


def _evaluate(cache_root,audio_root):
    out=Path(cache_root)/'kick-research-2026-10-09-evening/h17';rule=json.loads((out/'rule.json').read_text());inputs={}
    if rule['rule']!=frozen_rule():raise ValueError('predeclaration differs')
    for name,item in rule['inputs'].items():
        if sha(item['path'])!=item['sha256']:raise ValueError(f'frozen input changed:{name}')
        inputs[name]=json.loads(Path(item['path']).read_text())
    h10,h16=inputs['h10'],inputs['h16_compact']
    if not h10['complete'] or not h16['complete'] or h16['full_trial_sha256']!=inputs['h16_verification']['trial_sha256']:raise ValueError('completed audited components required')
    for report in (h10,h16):
        for name,digest in report['source_sha256'].items():
            if sha(ROOT/'tools/audio_analysis/eval'/name)!=digest:raise ValueError('component source changed')
    def forbidden(*a,**k):raise RuntimeError('no fit/audio/extraction allowed')
    with patch.object(cached,'read_audio',side_effect=forbidden),patch.object(cached,'fusion_features',side_effect=forbidden):
        records=runner.load_records(Path(audio_root),inputs['baseline'],Path(cache_root)/'kick-trajectory-2026-10-09/features')
    records,expanded=append_reviews(records,inputs['expanded']);original=[dict(r,features=r['features'][:,:15]) for r in records]
    records,coverage=add_training_coverage(original,inputs['additional'])
    if coverage!=h10['training_coverage']:raise ValueError('training coverage changed')
    evaluation_hash=hashlib.sha256(json.dumps([(r['source'],r['ref']) for r in records],sort_keys=True).encode()).hexdigest()
    if evaluation_hash!=h10['evaluation_source_ref_sha256']:raise ValueError('original381 evaluation objects changed')
    baseline=next(v for v in inputs['previous']['variants'] if v['variant']=='linear_15')
    covered=next(v for v in h10['variants'] if v['variant']=='coverage_linear15');frozen_kernel=h16['variants'][-1]
    fitter=FrozenFitter(covered,frozen_kernel,Path(cache_root)/'kick-research-2026-10-09-evening/h16/models');predict=PredictionCache(out/'predictions')
    if len(fitter.linear)!=45:raise ValueError('expected45 training sets')
    report=dict(hypothesis='H17',rule=rule,rule_sha256=sha(out/'rule.json'),complete=False,variants=[],new_base_fits=0,training_coverage=coverage,
        evaluation_source_ref_sha256=evaluation_hash,expanded_coverage=expanded,source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in ('kick_kernel_blend.py','run_kick_kernel_blend_trial.py','test_kick_kernel_blend.py')},
        limitations='Frozen original381 evaluation; known73 training reuse. Reports contain exact kernel model references. Cached evaluation timing is not native inference timing. No Pattern,D2,reserved data or app integration.')
    (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n')
    with patch.object(kernel.SVC,'fit',side_effect=forbidden),patch.object(linear,'minimize',side_effect=forbidden):
        report['original_baseline_replay']=replay_frozen(original,baseline,linear.predict_score)
        controls=0
        for record in records:
            model=fitter.fit(records,record['track'],.5)
            for weight,variant in ((0,covered),(1,frozen_kernel)):
                model.update(kernel_logit_weight=weight,linear_logit_weight=1-weight)
                scores=predict(model,record['features']);old=next(r for r in variant['tracks'] if r['track']==record['track'])
                for key,cut in [('fixed_05',.5),('calibrated',old['calibration']['threshold']),('refined',old['refinement']['threshold'])]:
                    fires,passages=threshold_scores(record,scores,cut)
                    if fires!=old['kick_hops'][key] or passages!=old['scores'][key]:raise ValueError('endpoint frozen replay differs')
                    controls+=1
        report['endpoint_event_replays']=controls
        for weight in KERNEL_WEIGHTS:
            report['active_weight']=weight;(out/'trial.json').write_text(json.dumps(report,indent=2)+'\n')
            variant=runner.run_variant(records,f'kernel_blend_{weight:g}',lambda rows,held:fitter.fit(rows,held,weight),predict)
            add_cohort_results(variant,original);add_diagnostics(variant,records,baseline,covered,predict)
            variant['fold_audit']=audit_models(variant,records,original);variant['component_audit']=audit_components(variant)
            for row in variant['tracks']:
                row['covered_linear_delta']=row.pop('same_scorer_delta');row['comparison_to_covered_linear']=row.pop('comparison_to_same_scorer')
                old=next(r for r in frozen_kernel['tracks'] if r['track']==row['track']);delta=comparisons(old['scores']['refined'],row['scores']['refined'])
                row['comparison_to_kernel']=delta;row['kernel_delta']=dict(lost_labels70=sum(len(p['metrics']['70']['lost_labels_s']) for p in delta),recovered_labels70=sum(len(p['metrics']['70']['recovered_labels_s']) for p in delta))
            if variant['totals']['refined']['labels']!=381:raise ValueError('label count changed')
            report['variants'].append(variant);report.update(complete=len(report['variants'])==3,cached_model_requests=fitter.requests,prediction_cache=predict.statistics())
            (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n');print('TOTAL',weight,variant['totals']['refined']['tolerance_ms']['70'],variant['acceptance'],flush=True)
    # Freeze all original381 outputs before known development73 scoring.
    additional=dict(original381_trial_sha256=sha(out/'trial.json'),labels_sha256=rule['inputs']['additional']['sha256'],disclosure=frozen_rule()['additional73'],variants=[score_additional(v,inputs['additional']) for v in report['variants']])
    (out/'additional73.json').write_text(json.dumps(additional,indent=2)+'\n')
    return dict(complete=True,prediction_cache=predict.statistics())


def run(cache_root,audio_root):
    for key in ('OPENBLAS_NUM_THREADS','OMP_NUM_THREADS','VECLIB_MAXIMUM_THREADS'):
        if os.environ.get(key)!='1':raise ValueError('one active CPU thread required')
    out=Path(cache_root)/'kick-research-2026-10-09-evening/h17'
    try:result,cpu=kernel.cpu_limited(_evaluate,(cache_root,audio_root),120)
    except kernel.TrainingBudgetExceeded:
        receipt=dict(complete=False,reason='120CPU-second scoring limit; preserved partial report and prediction caches',protected_cpu_s=120)
    else:receipt=dict(result,protected_cpu_s=cpu)
    (out/'cpu_receipt.json').write_text(json.dumps(receipt,indent=2)+'\n');print(json.dumps(receipt),flush=True)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache-root',type=Path,default=Path('/Users/peterkiemann/.cache/manifold'))
    parser.add_argument('--audio-root',type=Path,default=Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio'))
    args=parser.parse_args();run(args.cache_root,args.audio_root)
