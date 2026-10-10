"""Three predeclared H11 raw/residual spectral-shape boosted-tree trials."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import time
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_spectral_shape import (CONFIGURATIONS, cached_shape,
    configured_features, feature_names, frozen_rule)
from .kick_attack_rejection import sha
from .kick_boosted_score import FoldFitter, predict_score
from .run_kick_boosted_trial import add_diagnostics
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons
from .run_kick_shape_trial import append_reviews, add_cohort_results


def run(cache_root, audio_root):
    for key in ('OPENBLAS_NUM_THREADS','VECLIB_MAXIMUM_THREADS','OMP_NUM_THREADS'):
        if os.environ.get(key)!='1':raise ValueError('single CPU thread environment required')
    out=cache_root/'kick-research-2026-10-09-evening/h11';rule_path=out/'rule.json'
    if not rule_path.exists() or json.loads(rule_path.read_text())!=frozen_rule():
        raise ValueError('approved exact rule must be frozen before fitting')
    baseline_path=cache_root/'kick-balance-audit-2026-10-09/trial.json'
    previous_path=cache_root/'kick-shape-2026-10-09/trial.json'
    tree_path=cache_root/'kick-research-2026-10-09-evening/boosted/trial.json'
    expanded_path=ROOT/'tests/fixtures/audio_labels/expanded_passages_2026-10-09.json'
    baseline,previous,trees=[json.loads(p.read_text()) for p in (baseline_path,previous_path,tree_path)]
    if sha(expanded_path)!=previous['expanded_review_sha256']:raise ValueError('annotations changed')
    for name,digest in previous['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name)!=digest:raise ValueError(f'upstream changed:{name}')
    def forbidden(*args,**kwargs):raise RuntimeError('base39 must use immutable existing cache')
    with patch.object(cached,'read_audio',side_effect=forbidden),patch.object(cached,'fusion_features',side_effect=forbidden):
        records=runner.load_records(audio_root,baseline,cache_root/'kick-trajectory-2026-10-09/features')
    records,coverage=append_reviews(records,json.loads(expanded_path.read_text()))
    if len(records)!=9:raise ValueError('all nine songs required')
    linear=next(v for v in previous['variants'] if v['variant']=='linear_15')
    tree=next(v for v in trees['variants'] if v['variant']=='boosted_depth3')
    extras=[];cache_reports=[]
    for record in records:
        value,meta=cached_shape(record,out/'features');extras.append(value);cache_reports.append(meta)
        print('features',record['track'],meta['candidates'],'stream_error',meta['spectral_stream_max_abs_error'],flush=True)
    fitter=FoldFitter(out/'models')
    report=dict(hypothesis='H11',rule=frozen_rule(),rule_sha256=sha(rule_path),
        reference_sha256={str(p):sha(p) for p in (baseline_path,previous_path,tree_path,expanded_path)},
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in ('kick_spectral_shape.py','run_kick_spectral_shape_trial.py','test_kick_spectral_shape.py','kick_boosted_score.py')},
        original_label_sha256=baseline['label_sha256'],coverage=coverage,feature_caches=cache_reports,
        variants=[],complete=False,
        limitations='Nine development songs and provisional visual labels; no reserved evaluation. Residual spectral shape describes changed spectral energy, not isolated kick energy. Python CPU and bounded retained arrays do not establish native callback latency or absence of per-hop allocations. Known additional73 labels not used for fitting, calibration, feature or configuration selection.')
    for configuration in CONFIGURATIONS:
        started=time.monotonic()
        configured=[dict(r,features=configured_features(r['features'][:,:15],e,configuration)) for r,e in zip(records,extras)]
        variant=runner.run_variant(configured,configuration,lambda rows,held:fitter.fit(rows,held,3),predict_score)
        add_cohort_results(variant,configured);add_diagnostics(variant,configured,linear)
        variant['feature_names']=list(feature_names(configuration))
        variant['comparison_to_tree15']=[]
        for row,old in zip(variant['tracks'],tree['tracks']):
            if row['track']!=old['track']:raise ValueError('tree baseline order changed')
            delta=comparisons(old['scores']['refined'],row['scores']['refined'])
            variant['comparison_to_tree15'].append(dict(track=row['track'],
                lost_labels70=sum(len(p['metrics']['70']['lost_labels_s']) for p in delta),
                recovered_labels70=sum(len(p['metrics']['70']['recovered_labels_s']) for p in delta),passages=delta))
        if variant['cohorts']['original']['refined']['labels']!=174 or variant['cohorts']['expanded']['refined']['labels']!=207:
            raise ValueError('cohort counts changed')
        variant['evaluation_wall_s']=time.monotonic()-started
        report['variants'].append(variant);report['fit_cache']=fitter.statistics()
        report['complete']=len(report['variants'])==len(CONFIGURATIONS)
        (out/'trial.json').write_text(json.dumps(report,indent=2)+'\n')
        print('TOTAL',configuration,variant['totals']['refined']['tolerance_ms']['70'],variant['acceptance'],fitter.statistics(),flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache-root',type=Path,default=Path('/Users/peterkiemann/.cache/manifold'))
    parser.add_argument('--audio-root',type=Path,default=Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio'))
    args=parser.parse_args();run(args.cache_root,args.audio_root)
