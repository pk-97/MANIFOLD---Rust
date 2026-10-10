"""Fixed leave-one-recording-out timbre matching; no app integration or tuning.

References include reviewed mix windows and corresponding source stems. All
versions/stems of the evaluated song are excluded together. Positive reference
windows end at label+10/25/40 ms. Negative references end at wider-unmatched v5
fires or RMS-hybrid fires inside reviewed kick-free master passages. Comparison
uses one fixed cosine-margin boundary (zero), without fitting thresholds.
"""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import read_audio
from .kick_timbre_features import timbre_features, match_templates
from .live_kick_baseline import sha
from .run_kick_dsp_experiments import development_sources, evaluate, aggregate

ROOT=Path(__file__).resolve().parents[3]
REFERENCE_PHASES=(.010,.025,.040)
REFERENCE_RMS_FLOOR=1e-4
MASTER_TO_SOURCE_OFFSET={'late_night':0.,'midnight_patience':.136375}


def completed_index(deadline,sr,hop,count):
    index=int(np.floor(deadline*sr/hop+1e-9))-1
    if not 0<=index<count:
        raise ValueError('deadline outside completed-hop feature grid')
    assert (index+1)*hop/sr<=deadline+1e-9
    return index


def reviewed_events(source,sr,hop,hybrid):
    scores=evaluate(source,[(i+1)*hop/sr for i in source['native_hops']])
    truth,missed,negatives=[],[],[]
    for score in scores:
        strict=score['accuracy_by_tolerance_ms']['50']
        pairs=score['association_early_35_late_200_ms']['pairs']
        paired={p['available_s'] for p in pairs}
        labels=sorted(set(strict['missed_times_s'])|{p['attack_s'] for p in pairs})
        assert len(labels)==score['labels']
        truth.extend(labels)
        missed.extend(strict['missed_times_s'])
        negatives.extend(sorted(set(strict['extra_times_s'])-paired))
    if source['group']=='new_masters':
        previous=next(t for t in hybrid['variants']['rms_presence']['tracks'] if t['track']==source['track'])
        assert previous['audio_sha256']==source['audio_sha256']
        for passage in source['passages']:
            if passage['kick_times_s']:
                continue
            negatives.extend((i+1)*hop/sr for i in previous['kick_hops']
                if passage['start_s']<=(i+1)*hop/sr<passage['end_s'])
    return dict(scores=scores,truth=sorted(set(truth)),missed=sorted(set(missed)),negative_times=sorted(set(negatives)))


def reference_sources(source,audio_root,registry):
    """Mixed drum stems are kick-containing references, never called isolated kicks."""
    if source['group']=='original_five':
        labels=list(csv.DictReader((ROOT/'tests/fixtures/audio_labels'/f"{source['track']}.csv").open()))
        if not all(abs(float(r['mix_time_s'])-float(r['drums_time_s']))<1e-9 for r in labels):
            raise ValueError('original mix/drum clock mapping needs explicit review')
        return [(audio_root/source['track']/'drums.wav','mixed_drums',True,True,None),
                (audio_root/source['track']/'bass.wav','bass',False,True,None)]
    folder,kick,bass,drums={
        'late_night':('LATE NIGHT STEMS','Late Night - Kicks Stem.wav','Late Night - Bass and Sub Stem.wav','Late Night - Drums Stem.wav'),
        'midnight_patience':('MIDNIGHT PATIENCE STEMS','Kick.wav','Bass and Sub.wav','Drums.wav')
    }[source['track']]
    entry=next(s for s in registry['sets'] if s['folder']==folder)
    assert entry['split']=='development'
    rows=[]
    for file,role,pos,neg in ((kick,'kick',True,False),(bass,'bass',False,True),(drums,'other_drums',False,True)):
        relative=f'{folder}/{file}'
        expected=next(f['sha256'] for f in entry['files'] if f['file']==relative)
        rows.append((Path(registry['source_root'])/relative,role,pos,neg,expected))
    return rows


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--audio-root',required=True,type=Path)
    ap.add_argument('--hybrid-report',required=True,type=Path)
    ap.add_argument('--out',required=True,type=Path)
    args=ap.parse_args()
    args.out.parent.mkdir(parents=True,exist_ok=True)
    hybrid=json.loads(args.hybrid_report.read_text())
    registry=json.loads((Path(__file__).parent/'additional_stem_sources.json').read_text())
    source_list=development_sources(args.audio_root)
    bank,metadata,tracks,files,skipped=[],[],[],[],[]
    seen=set()
    for source in source_list:
        path=Path(source['audio_path'])
        if sha(path)!=source['audio_sha256']:
            raise ValueError('mix differs from reviewed recording')
        sr,samples=read_audio(path)
        hop=round(sr*256/48000)
        start=time.process_time()
        features=timbre_features(samples,sr,hop)
        feature_cpu=time.process_time()-start
        events=reviewed_events(source,sr,hop,hybrid)
        track=dict(source=source,sample_rate=sr,hop=hop,features=features,
            feature_cpu_s=feature_cpu,duration_s=len(samples)/sr,**events)
        tracks.append(track)
        np.savez(args.out.parent/(source['track']+'_mix_features.npz'),features=features,sample_rate=sr,hop=hop)

        def append_references(audio,rate,grid_hop,vectors,file,role,positive,negative,offset,expected):
            digest=sha(file)
            if expected is not None and digest!=expected:
                raise ValueError('reference source hash changed')
            files.append(dict(track=source['track'],path=str(file),role=role,sha256=digest,
                sample_rate=rate,master_minus_source_s=offset,checked_against_previous_hash=expected is not None))
            requests=[]
            if positive:
                requests.extend((t-offset+phase,True,t,phase) for t in events['truth'] for phase in REFERENCE_PHASES)
            if negative:
                requests.extend((t-offset,False,t,0.) for t in events['negative_times'])
            for deadline,label,reference_time,phase in requests:
                index=completed_index(deadline,rate,grid_hop,len(vectors))
                key=(str(file),label,index)
                if key in seen:
                    continue
                seen.add(key)
                end=(index+1)*grid_hop
                excerpt=audio[max(0,end-round(.060*rate)):end]
                rms=float(np.sqrt(np.mean(excerpt*excerpt)))
                if rms<REFERENCE_RMS_FLOOR or not np.any(vectors[index]):
                    skipped.append(dict(track=source['track'],role=role,positive=label,time_s=deadline,reason='below fixed reference RMS floor or numerical silence'))
                    continue
                bank.append(vectors[index].copy())
                metadata.append(dict(track=source['track'],role=role,positive=label,path=str(file),
                    available_s=end/rate,master_reference_s=reference_time,phase_s=phase,rms=rms))
        append_references(samples,sr,hop,features,path,'mix',True,True,0.,source['audio_sha256'])
        for file,role,positive,negative,expected in reference_sources(source,args.audio_root,registry):
            rate,audio=read_audio(file)
            grid_hop=round(rate*256/48000)
            vectors=timbre_features(audio,rate,grid_hop)
            append_references(audio,rate,grid_hop,vectors,file,role,positive,negative,
                MASTER_TO_SOURCE_OFFSET.get(source['track'],0.),expected)
        print(source['track'],'reference construction complete',flush=True)

    templates=np.asarray(bank)
    labels=np.asarray([m['positive'] for m in metadata],dtype=bool)
    groups=np.asarray([m['track'] for m in metadata])
    np.savez(args.out.parent/'templates.npz',templates=templates,labels=labels,groups=groups)
    output=[]
    for track in tracks:
        source=track['source'];name=source['track'];sr=track['sample_rate'];hop=track['hop'];features=track['features']
        def classify(deadline):
            index=completed_index(deadline,sr,hop,len(features))
            start=time.process_time()
            result=match_templates(features[index],templates,labels,groups,name)
            cpu=time.process_time()-start
            assert all(metadata[result[key]]['track']!=name for key in ('positive_index','negative_index'))
            return dict(available_s=(index+1)*hop/sr,accept=result['margin']>=0.,match_cpu_s=cpu,**result)
        # Apply the same rule to every native fire, including review margins.
        # Accuracy claims remain restricted to the existing reviewed passages.
        decisions=[dict(original_fire_s=(i+1)*hop/sr,**classify((i+1)*hop/sr)) for i in source['native_hops']]
        filtered=[d['original_fire_s'] for d in decisions if d['accept']]
        scores=evaluate(source,filtered)
        label_probes=[dict(label_s=t,baseline_missed=t in track['missed'],
            probes={str(ms):classify(t+ms/1000) for ms in (20,35,50)}) for t in track['truth']]
        missed=[p for p in label_probes if p['baseline_missed']]
        kick_free=[]
        if source['group']=='new_masters':
            for passage in source['passages']:
                if not passage['kick_times_s']:
                    kick_free.extend(dict(time_s=t,passage=passage['id'],**classify(t))
                        for t in track['negative_times'] if passage['start_s']<=t<passage['end_s'])
        active=groups!=name
        row=dict(track=name,group=source['group'],audio_sha256=source['audio_sha256'],
            feature_cpu_s=track['feature_cpu_s'],duration_s=track['duration_s'],
            training_tracks=sorted(set(groups[active].tolist())),
            training_positive=int(np.sum(labels[active])),training_negative=int(np.sum(~labels[active])),
            excluded_same_track=int(np.sum(~active)),baseline_scores=track['scores'],scores=scores,
            decisions=decisions,label_probes=label_probes,missed_probes=missed,kick_free_probes=kick_free)
        output.append(row)
        print(name,[{k:s['accuracy_by_tolerance_ms']['50'][k] for k in ('matched','missed','extra')} for s in scores],flush=True)
    result=dict(method=__doc__,tracks=output,totals=aggregate(output),reference_files=files,
        templates=metadata,skipped_references=skipped,
        settings=dict(reference_phases_s=REFERENCE_PHASES,reference_rms_floor=REFERENCE_RMS_FLOOR,
            accept_margin=0.,fingerprint='20 geometric bands 45-8000 Hz; fourth-root power; four consecutive trailing 2048-at-48k Hann spectra; whole-sequence L2 normalization.'),
        source_sha256={name:sha(Path(__file__).parent/name) for name in ('run_kick_timbre_trial.py','kick_timbre_features.py','kick_tonal_experiment.py','run_kick_dsp_experiments.py','master_kick_comparison.py','live_kick_baseline.py','kick_attack_rejection.py')},
        labels_sha256={p.name:sha(p) for p in (ROOT/'tests/fixtures/audio_labels').glob('*') if p.suffix in ('.csv','.json')},
        hybrid_report_sha256=sha(args.hybrid_report),
        limitations=[
            'Leave-one-recording-out development comparison, not untouched validation. All same-song stems and versions excluded; Miracle and Heavy On Mind untouched.',
            'Reference labels are provisional visual/source-assisted estimates. Original drum stems contain other drums; negative stem windows can include separation leakage or tails. No isolated-kick purity or new auditory validation claimed.',
            'Master/source mappings use existing clock evidence; Late Night uses approximate shared origin and Midnight +0.136375 s. Masters differ from stems; no waveform reconstruction or new warp was performed.',
            'Reference class sizes vary; nearest-template matching may favour densely represented classes. No class balancing or tuning was added after observing results.',
            'Fingerprint spans about 59 ms of past audio; references sample three fixed event ages, while test fires use actual availability. No alignment search, future samples or extra waiting.',
            'Only existing fires are filtered; there is no new candidate pathway. Missed-label probes are oracle classifications, not recovered trigger recall.',
            'After the first fixed-rule outcome, the identical classifier was additionally sampled at all labelled onsets plus 20/35/50 ms and at secondary kick-free fires to diagnose timing and false acceptance. No features, references or decision boundary changed.',
            'Baseline native event lists are reused from frozen reports; no claim of a new native runtime evaluation.',
            'Batch Python timings and allocations are not native callback deadlines. No GPU, neural network or app integration.'])
    args.out.write_text(json.dumps(result,indent=2)+'\n')


if __name__=='__main__':main()
