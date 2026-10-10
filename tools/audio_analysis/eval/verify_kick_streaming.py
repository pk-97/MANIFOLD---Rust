"""Full-recording bounded-state parity and CPU benchmark, without app integration."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import time

import numpy as np
import soundfile as sf

from .kick_attack_rejection import sha
from .kick_boosted_score import predict_score as tree_predict
from .kick_streaming_reference import DecisionStream, FeatureStream
from .run_kick_dsp_experiments import ROOT, development_sources
from .run_kick_fusion_trial import predict_score as linear_predict


def retained_decision_bytes(stream):
    arrays = [a for a in vars(stream).values() if isinstance(a,np.ndarray)]
    if stream.kind == 'tree':
        arrays += [a for tree in stream.trees for a in tree]
    return sum(a.nbytes for a in arrays)


def run(linear_path,tree_path,cache,audio_root,out):
    linear_report,tree_report = [json.loads(p.read_text()) for p in (linear_path,tree_path)]
    linear = next(v for v in linear_report['variants'] if v['variant']=='linear_15')
    tree = next(v for v in tree_report['variants'] if v['variant']=='boosted_depth3')
    for report in (linear_report,tree_report):
        for name,digest in report['source_sha256'].items():
            if sha(Path(__file__).parent/name)!=digest:
                raise ValueError(f'reference implementation changed:{name}')
    sources = development_sources(audio_root)
    for row in json.loads((ROOT/'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json').read_text())['tracks']:
        sources.append(dict(track=row['track'],audio_path=row['master_path'],audio_sha256=row['audio_sha256']))
    source_by_track = {s['track']:s for s in sources}
    cache_by_hash = {}
    for metadata in cache.glob('*.json'):
        meta=json.loads(metadata.read_text())
        cache_by_hash[meta['signature']['audio_sha256']] = (metadata,meta)
    rows=[]
    for base,current in zip(linear['tracks'],tree['tracks']):
        assert base['track']==current['track']
        source=source_by_track[base['track']]
        metadata,meta=cache_by_hash[source['audio_sha256']]
        path=Path(source['audio_path'])
        if sha(path)!=meta['signature']['audio_sha256']:
            raise ValueError('audio identity differs')
        data_path=metadata.with_suffix('.npz')
        if sha(data_path)!=meta['data_sha256']:
            raise ValueError('cached feature identity differs')
        with np.load(data_path,allow_pickle=False) as z:
            candidates=z['candidates'];available=z['available'];features=z['features'][:,:15]
        stream=FeatureStream(meta['sample_rate'])
        models={'linear':base,'tree':current}
        decision={name:DecisionStream(row['model'],row['refinement']['threshold'],
                    meta['sample_rate'],stream.hop) for name,row in models.items()}
        expected_scores={'linear':linear_predict(base['model'],features),
                         'tree':tree_predict(current['model'],features)}
        errors=np.zeros(15);score_errors=dict(linear=0.,tree=0.)
        events=dict(linear=[],tree=[]);feature_cpu=0;decision_cpu=dict(linear=0,tree=0)
        wall_hops=[];cursor=0
        initial_bytes=stream.retained_array_bytes()
        with sf.SoundFile(path) as audio:
            if audio.samplerate != stream.sample_rate:
                raise ValueError('native rate differs')
            for i in range(len(audio)//stream.hop):
                block=audio.read(stream.hop,dtype='float64',always_2d=True).mean(axis=1)
                wall_start=time.perf_counter_ns();start=time.process_time_ns()
                result=stream.push_hop(block)
                feature_cpu+=time.process_time_ns()-start
                if result:
                    candidate,deadline,row=result
                    if cursor>=len(candidates) or candidate!=candidates[cursor] or deadline!=available[cursor]:
                        raise ValueError(f'stream candidate/time mismatch:{base["track"]}/{cursor}')
                    errors=np.maximum(errors,np.abs(row-features[cursor]))
                    for name,runtime in decision.items():
                        start=time.process_time_ns();score,fire=runtime.push(row,deadline)
                        decision_cpu[name]+=time.process_time_ns()-start
                        score_errors[name]=max(score_errors[name],abs(score-expected_scores[name][cursor]))
                        if fire:events[name].append(deadline)
                    cursor+=1
                wall_hops.append((time.perf_counter_ns()-wall_start)/1e6)
        if cursor!=len(candidates) or errors.max()>1e-8 or max(score_errors.values())>1e-8:
            raise ValueError(f'stream feature/score parity failed:{base["track"]}/{errors.max()}')
        for name,row in models.items():
            if events[name]!=row['kick_hops']['refined']:
                raise ValueError(f'stream emitted events differ:{base["track"]}/{name}')
        if stream.retained_array_bytes()!=initial_bytes:
            raise ValueError('retained feature state grew')
        duration=meta['duration_s'];cpu_seconds=feature_cpu/1e9
        row=dict(track=base['track'],sample_rate=stream.sample_rate,hop=stream.hop,
            audio_sha256=source['audio_sha256'],audio_seconds=duration,complete_hops=len(wall_hops),
            candidate_count=cursor,candidate_and_event_parity_exact=True,
            feature_max_abs_error=errors.tolist(),score_max_abs_error=score_errors,
            feature_cpu_s=cpu_seconds,decision_cpu_s={n:v/1e9 for n,v in decision_cpu.items()},
            feature_plus_tree_cpu_percent=100*(cpu_seconds+decision_cpu['tree']/1e9)/duration,
            feature_state_array_bytes=initial_bytes,
            decision_state_array_bytes={n:retained_decision_bytes(s) for n,s in decision.items()},
            observation_ms=1000*stream.horizon*stream.hop/stream.sample_rate,
            hop_duration_ms=1000*stream.hop/stream.sample_rate,
            wall_ms_including_both_decisions_and_verification=dict(zip(('median','p95','p99','max'),
                np.quantile(wall_hops,[.5,.95,.99,1]).tolist())))
        rows.append(row)
        report=dict(method=__doc__,platform=platform.platform(),python=platform.python_version(),
            source_sha256={n:sha(Path(__file__).parent/n) for n in
                ('kick_streaming_reference.py','verify_kick_streaming.py','test_kick_streaming_reference.py')},
            reference_sha256=dict(linear=sha(linear_path),tree=sha(tree_path)),tracks=rows,
            limitations='Python/SciPy CPU proof, not native callback or display latency. Decoding excluded '
                'from measured DSP CPU. Reported wall hop includes both scorers and parity checks. '
                'Retained-array bytes exclude Python object overhead; temporary arrays have fixed bounds '
                'but allocate per hop. Verification retains output/timing evidence outside detector state. '
                'No live integration, no changed acoustic model and no reserved recordings.')
        out.parent.mkdir(parents=True,exist_ok=True);out.write_text(json.dumps(report,indent=2)+'\n')
        print(base['track'],'parity exact; CPU%',round(row['feature_plus_tree_cpu_percent'],3),
              'max feature error',errors.max(),flush=True)
    return report


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('linear','tree','cache','audio-root','out'):parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();run(args.linear,args.tree,args.cache,args.audio_root,args.out)
