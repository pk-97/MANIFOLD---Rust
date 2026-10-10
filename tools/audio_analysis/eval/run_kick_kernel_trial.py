"""H16 three frozen kernels; stop and preserve evidence at any CPU limit."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
from unittest.mock import patch

from . import kick_trajectory_features as cached,run_kick_trajectory_trial as runner
from .kick_kernel_score import GAMMAS,FoldFitter,TrainingBudgetExceeded,frozen_rule,predict_score
from .kick_boosted_score import training_key
from .kick_attack_rejection import sha
from .kick_training_coverage import add_training_coverage
from .run_kick_training_coverage_trial import add_diagnostics,audit_models
from .run_kick_shape_trial import append_reviews,add_cohort_results
from .run_kick_dsp_experiments import ROOT


def run(cache_root,audio_root):
    for key in ('OPENBLAS_NUM_THREADS','VECLIB_MAXIMUM_THREADS','OMP_NUM_THREADS'):
        if os.environ.get(key)!='1':raise ValueError('one active CPU thread required')
    evening=cache_root/'kick-research-2026-10-09-evening';out=evening/'h16';rule_path=out/'rule.json'
    rule=json.loads(rule_path.read_text())
    if rule['rule']!=frozen_rule():raise ValueError('predeclaration differs')
    inputs={}
    for key,item in rule['inputs'].items():
        if sha(Path(item['path']))!=item['sha256']:raise ValueError(f'frozen input changed:{key}')
        inputs[key]=json.loads(Path(item['path']).read_text())
    for name,digest in inputs['coverage']['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name)!=digest:raise ValueError(f'coverage source changed:{name}')
    def forbidden(*a,**k):raise RuntimeError('cached15 only; no audio decoding')
    with patch.object(cached,'read_audio',side_effect=forbidden),patch.object(cached,'fusion_features',side_effect=forbidden):
        records=runner.load_records(audio_root,inputs['baseline'],cache_root/'kick-trajectory-2026-10-09/features')
    records,expanded=append_reviews(records,inputs['expanded']);originals=[dict(r,features=r['features'][:,:15]) for r in records]
    records,coverage=add_training_coverage(originals,inputs['additional'])
    if coverage!=inputs['coverage']['training_coverage']:raise ValueError('H10 training coverage receipt differs')
    evaluation_hash=hashlib.sha256(json.dumps([(r['source'],r['ref']) for r in records],sort_keys=True).encode()).hexdigest()
    if evaluation_hash!=inputs['coverage']['evaluation_source_ref_sha256']:raise ValueError('original evaluation changed')
    for row in inputs['coverage']['variants'][0]['tracks']:
        for model in [row['model']]+[f['model'] for f in row['calibration']['inner_folds']]:
            expected=[list(training_key(r)) for r in records if r['track'] in model['training_tracks']]
            if expected!=model['training_input_keys']:raise ValueError('H10 exact training arrays differ')
    reference=next(v for v in inputs['previous']['variants'] if v['variant']=='linear_15')
    covered_linear=next(v for v in inputs['coverage']['variants'] if v['variant']=='coverage_linear15')
    fitter=FoldFitter(out/'models')
    report=dict(rule=rule,rule_sha256=sha(rule_path),hypothesis='H16',complete=False,variants=[],training_coverage=coverage,
        evaluation_source_ref_sha256=evaluation_hash,exact_H10_training_inputs=True,expanded_coverage=expanded,
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in ('kick_kernel_score.py','run_kick_kernel_trial.py','test_kick_kernel_score.py')},
        limitations='Original381 evaluation; accepted73 are development training coverage within excluded-family folds. NoD2 training, reserved data or app integration. Any CPU-limited fit is discarded. Inference cost is not a native callback guarantee.')
    for gamma in GAMMAS:
        report['active_gamma']=gamma
        (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n')
        try:
            variant=runner.run_variant(records,f'kernel_gamma_{gamma:g}',lambda rows,held:fitter.fit(rows,held,gamma),predict_score)
        except TrainingBudgetExceeded as error:
            report.update(stopped_reason=str(error),feasibility='training_CPU_limit',fit_cache=fitter.statistics())
            (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n')
            print('BOUNDED_PARTIAL',gamma,str(error),fitter.statistics(),flush=True)
            return report
        except Exception as error:
            report.update(stopped_reason=repr(error),feasibility='error',fit_cache=fitter.statistics())
            (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n');raise
        add_cohort_results(variant,originals)
        add_diagnostics(variant,records,reference,covered_linear,predict_score)
        variant['fold_audit']=audit_models(variant,records,originals)
        if variant['totals']['refined']['labels']!=381:raise ValueError('evaluation labels changed')
        report['variants'].append(variant);report['fit_cache']=fitter.statistics()
        report['complete']=len(report['variants'])==len(GAMMAS)
        (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n')
        print('TOTAL',gamma,variant['totals']['refined']['tolerance_ms']['70'],variant['acceptance'],flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache-root',type=Path,default=Path('/Users/peterkiemann/.cache/manifold'))
    parser.add_argument('--audio-root',type=Path,default=Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio'))
    args=parser.parse_args();run(args.cache_root,args.audio_root)
