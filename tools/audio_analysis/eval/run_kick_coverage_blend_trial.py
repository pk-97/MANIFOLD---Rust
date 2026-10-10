"""Three fixed H10 covered linear/interaction blends; no new base fits."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import time
from unittest.mock import patch

from . import kick_trajectory_features as cached
from . import kick_interaction_score as interaction
from . import run_kick_fusion_trial as linear
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_coverage_blend import FrozenFitter, INTERACTION_WEIGHTS, component_signature, predict_score
from .kick_training_coverage import add_training_coverage
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons
from .run_kick_shape_trial import add_cohort_results, append_reviews
from .run_kick_training_coverage_trial import add_diagnostics, audit_models, replay_frozen, score_additional


def audit_components(variant):
    checked=0
    for row in variant['tracks']:
        for model,excluded in [(row['model'],{row['track']})]+[(f['model'],{row['track'],f['validation_track']})
                for f in row['calibration']['inner_folds']]:
            for name in ('linear','interaction'):
                component=model[name]
                if excluded.intersection(component['training_tracks']):
                    raise ValueError('held-out family leaked into cached component')
                for key in ('training_tracks','training_input_keys','mean','scale'):
                    if component[key]!=model[key]:raise ValueError('component/root provenance differs')
                if component_signature(component)!=model['component_sha256'][name]:
                    raise ValueError('cached component signature differs')
            checked+=1
    return dict(blend_models_checked=checked,component_models_checked=checked*2,
                all_component_exclusions_keys_normalisations_and_hashes_exact=True)


def run(audio_root,cache,rule_path,out):
    for name in ('OPENBLAS_NUM_THREADS','OMP_NUM_THREADS','VECLIB_MAXIMUM_THREADS'):
        if os.environ.get(name)!='1':raise ValueError('one CPU thread required')
    rule=json.loads(rule_path.read_text())
    if rule['status']!='predeclared' or rule['configurations']!=[
            dict(interaction_logit_weight=w,linear_logit_weight=1-w) for w in INTERACTION_WEIGHTS]:
        raise ValueError('blend predeclaration differs')
    if sha(Path(rule['h10']['path']))!=rule['h10']['sha256']:raise ValueError('H10 report changed')
    h10=json.loads(Path(rule['h10']['path']).read_text())
    if not h10['complete']:raise ValueError('completed H10 components required')
    inputs={}
    for name,item in rule['inputs'].items():
        if sha(Path(item['path']))!=item['sha256']:raise ValueError(f'frozen input changed: {name}')
        inputs[name]=json.loads(Path(item['path']).read_text())
    for name,digest in h10['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name)!=digest:raise ValueError(f'H10 source changed: {name}')
    def forbidden(*args,**kwargs):raise RuntimeError('no new base fits/audio/features allowed')
    with patch.object(cached,'read_audio',side_effect=forbidden),patch.object(cached,'fusion_features',side_effect=forbidden):
        records=runner.load_records(audio_root,inputs['baseline'],cache)
    records,expanded=append_reviews(records,inputs['expanded'])
    original=[dict(r,features=r['features'][:,:15]) for r in records]
    records,coverage=add_training_coverage(original,inputs['additional'])
    if coverage!=h10['training_coverage']:raise ValueError('covered training inputs differ from H10')
    evaluation_hash=hashlib.sha256(json.dumps([(r['source'],r['ref']) for r in records],sort_keys=True).encode()).hexdigest()
    if evaluation_hash!=h10['evaluation_source_ref_sha256']:raise ValueError('original381 scoring objects changed')
    baseline=next(v for v in inputs['previous']['variants'] if v['variant']=='linear_15')
    covered_linear=next(v for v in h10['variants'] if v['variant']=='coverage_linear15')
    covered_interaction=next(v for v in h10['variants'] if v['variant']=='coverage_interaction15')
    replays=[replay_frozen(original,baseline,linear.predict_score),
        replay_frozen(records,covered_linear,linear.predict_score),
        replay_frozen(records,covered_interaction,interaction.predict_score)]
    fitter=FrozenFitter(covered_linear,covered_interaction)
    if len(fitter.linear)!=45:raise ValueError('expected45 cached training sets')
    out.parent.mkdir(parents=True,exist_ok=True)
    files=('kick_coverage_blend.py','run_kick_coverage_blend_trial.py','test_kick_coverage_blend.py')
    report=dict(hypothesis=rule,rule_sha256=sha(rule_path),h10_sha256=sha(Path(rule['h10']['path'])),
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in files},
        upstream_source_sha256=h10['source_sha256'],complete=False,variants=[],new_base_fits=0,
        frozen_replays=replays,cached_training_sets=len(fitter.linear),training_coverage=coverage,
        expanded_coverage=expanded,evaluation_source_ref_sha256=evaluation_hash,
        limitations=rule['additional73']+' Offline vectorised decision timings, not callback guarantees. No reserved or D2 data.')
    out.write_text(json.dumps(report,indent=2)+'\n')
    # Guard even accidental calls into either base optimiser throughout evaluation.
    with patch.object(linear,'minimize',side_effect=forbidden),patch.object(interaction,'minimize',side_effect=forbidden):
        for weight in INTERACTION_WEIGHTS:
            started=time.process_time()
            variant=runner.run_variant(records,f'coverage_blend_{weight:g}',
                lambda rows,held:fitter.fit(rows,held,weight),predict_score)
            add_cohort_results(variant,original)
            add_diagnostics(variant,records,baseline,covered_linear,predict_score)
            variant['fold_audit']=audit_models(variant,records,original)
            variant['component_audit']=audit_components(variant)
            if variant['totals']['refined']['labels']!=381 or variant['cohorts']['expanded']['refined']['labels']!=207:
                raise ValueError('original381 cohort changed')
            for row,record in zip(variant['tracks'],records):
                row['covered_linear_delta']=row.pop('same_scorer_delta')
                row['comparison_to_covered_linear']=row.pop('comparison_to_same_scorer')
                old=next(r for r in covered_interaction['tracks'] if r['track']==row['track'])
                compared=comparisons(old['scores']['refined'],row['scores']['refined'])
                row['comparison_to_covered_interaction']=compared
                row['covered_interaction_delta']=dict(lost_labels70=sum(len(p['metrics']['70']['lost_labels_s']) for p in compared),
                    recovered_labels70=sum(len(p['metrics']['70']['recovered_labels_s']) for p in compared))
                row['decision_cost']=dict(candidates=len(record['features']),cpu_s=row['prediction_cpu_s'],
                    microseconds_per_candidate=row['prediction_cpu_s']/len(record['features'])*1e6,
                    cpu_s_per_audio_s=row['prediction_cpu_s']/record['cache_metadata']['duration_s'],
                    input_features=15,interaction_features=135,
                    claim='Offline vectorised scorer only; no new DSP or native realtime benchmark.')
            variant['evaluation_cpu_s']=time.process_time()-started
            variant['decision_cpu_s']=sum(r['prediction_cpu_s'] for r in variant['tracks'])
            report['variants'].append(variant);report['cached_model_requests']=fitter.requests
            report['complete']=len(report['variants'])==len(INTERACTION_WEIGHTS)
            out.write_text(json.dumps(report,indent=2)+'\n')
            print('TOTAL',variant['variant'],variant['totals']['refined']['tolerance_ms']['70'],variant['acceptance'],flush=True)
    # All three original381 results are immutable before the known73 are scored.
    additional=dict(original381_trial_sha256=sha(out),labels_sha256=rule['inputs']['additional']['sha256'],
        disclosure=rule['additional73'],variants=[score_additional(v,inputs['additional']) for v in report['variants']])
    (out.parent/'additional73.json').write_text(json.dumps(additional,indent=2)+'\n')
    print('ADDITIONAL73',[(v['variant'],v['totals']['accuracy_by_tolerance_ms']['70'],v['negative_cores']) for v in additional['variants']],flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root','cache','rule','out'):parser.add_argument('--'+name,type=Path,required=True)
    a=parser.parse_args();run(a.audio_root,a.cache,a.rule,a.out)
