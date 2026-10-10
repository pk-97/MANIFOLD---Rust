"""Frozen-model stress checks; synthetic controls never select a model or cutoff."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
from scipy.signal import butter,sosfilt

from .kick_attack_rejection import sha
from .kick_boosted_score import predict_score as tree_predict
from .kick_fusion_bandwise import fusion_features
from .kick_fusion_calibration import select_fires
from .kick_hybrid_experiment import controls
from .live_kick_baseline import score_events
from .run_kick_fusion_trial import predict_score as linear_predict


def cases(sr):
    rows=controls(sr);bass=rows[0][1];kicks=rows[2][1]
    hats=np.zeros_like(bass);rng=np.random.default_rng(14)
    t=np.arange(round(.050*sr))/sr
    burst=sosfilt(butter(2,[1000,8000],btype='bandpass',fs=sr,output='sos'),rng.normal(size=len(t)))
    burst*=.25*np.exp(-t/.008)
    gate=np.zeros_like(bass)
    for onset in (.5,1,1.5,2):
        first=round(onset*sr);hats[first:first+len(burst)]+=burst
        n=round(.25*sr);age=np.arange(n)/sr
        gate[first:first+n]=np.minimum(age/.003,1)
    return rows+[('silence',np.zeros_like(bass),[]),('hats_only',hats,[]),
        ('hats_over_stationary_bass',hats+bass,[]),('hats_with_bass_notes',hats+bass*gate,[]),
        ('changing_bass_to_kicks',np.r_[bass[:2*sr],kicks[:2*sr]],[2.5,3.,3.5])]


def run(linear_path,tree_path,out):
    reports=[json.loads(p.read_text()) for p in (linear_path,tree_path)]
    variants=[next(v for v in d['variants'] if v['variant']==name)
              for d,name in zip(reports,('linear_15','boosted_depth3'))]
    rows=[]
    for sr in (44100,48000):
        for name,samples,truth in cases(sr):
            c,a,f,hop=fusion_features(samples,sr)
            for variant,predict in zip(variants,(linear_predict,tree_predict)):
                fold_rows=[]
                for fold in variant['tracks']:
                    scores=predict(fold['model'],f)
                    fires=select_fires(scores,a,sr,hop,fold['refinement']['threshold'])
                    times=[(i+1)*hop/sr for i in fires]
                    fold_rows.append(dict(fold_excluded=fold['track'],emitted_times_s=times,
                        complete=score_events(times,truth,[]),
                        after_startup=score_events(times,truth,[dict(start_s=0,end_s=.3,reason='separate startup diagnostic')]),
                        startup_emissions_s=[t for t in times if t<.3]))
                metrics=[r['complete']['accuracy_by_tolerance_ms']['70'] for r in fold_rows]
                rows.append(dict(case=name,sample_rate=sr,variant=variant['variant'],truth_s=truth,
                    matched70_range=[min(m['matched'] for m in metrics),max(m['matched'] for m in metrics)],
                    extra70_range=[min(m['extra'] for m in metrics),max(m['extra'] for m in metrics)],folds=fold_rows))
            print(sr,name,[(r['variant'],r['matched70_range'],r['extra70_range']) for r in rows[-2:]],flush=True)
    report=dict(method=__doc__,references=dict(linear=sha(linear_path),tree=sha(tree_path)),
        source_sha256={n:sha(Path(__file__).parent/n) for n in ('verify_kick_controls.py','kick_hybrid_experiment.py')},
        rows=rows,limitations='Constructed CPU-only controls test known acoustic combinations and cold-start behaviour, not commercial-song accuracy. Every frozen outer model and its original cutoff is reported; no synthetic-driven selection or fitting. Full startup events remain visible, separately from the older300ms-excluded comparison.')
    out.parent.mkdir(parents=True,exist_ok=True);out.write_text(json.dumps(report,indent=2)+'\n')
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('linear','tree','out'):parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();run(args.linear,args.tree,args.out)
