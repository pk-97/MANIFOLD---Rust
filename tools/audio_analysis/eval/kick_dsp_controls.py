"""Bounded negative controls and source-stem combinations for DSP diagnosis.

Stem sums are controlled interventions, not reconstructions of finished masters.
Counts in source passages are diagnostic only, not master-label accuracy scores.
"""
from pathlib import Path
import argparse
import json
import numpy as np

from .kick_attack_rejection import read_audio, causal_features
from .kick_dsp_experiments import detect_v5, detect_temporal
from .kick_pcen_experiment import detect as detect_pcen
from .kick_tonal_experiment import detect as detect_tonal
from .live_kick_baseline import sha

ROOT = Path(__file__).resolve().parents[3]
DETECTORS = dict(baseline=lambda a,s,e,h:detect_v5(e,s,h),
                 temporal=detect_temporal,pcen=detect_pcen,tonal=detect_tonal)


def probe(audio, sr, windows):
    env,hop=causal_features(audio,sr)
    rows=[]
    for name,fn in DETECTORS.items():
        fires=fn(audio,sr,env,hop)
        times=[(i+1)*hop/sr for i in fires]
        rows.append(dict(variant=name,windows=[dict(id=w['id'],start_s=w['start_s'],end_s=w['end_s'],
            fire_times_s=[t for t in times if w['start_s']<=t<w['end_s']]) for w in windows]))
    return rows


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--out',type=Path,required=True)
    args=ap.parse_args()
    sr=48000;t=np.arange(sr*4)/sr
    stationary=(.25*np.sin(2*np.pi*80*t)+.35*np.sin(2*np.pi*160*t)
                +.25*np.sin(2*np.pi*240*t))*np.minimum(t/.02,1)
    rows=[]
    for name,audio in [('stationary_harmonic_bass',stationary),
                       ('amplitude_modulated_bass',stationary*(.55+.45*np.sin(2*np.pi*5*t)))]:
        result=probe(audio,sr,[dict(id='after_startup_no_kicks',start_s=.5,end_s=4.)])
        rows.append(dict(source=name,ground_truth='No kicks anywhere; first 0.5s omitted for startup.',results=result))
        print(name,{r['variant']:len(r['windows'][0]['fire_times_s']) for r in result},flush=True)
    registry=json.loads((ROOT/'tools/audio_analysis/eval/additional_stem_sources.json').read_text())
    selections=[('LATE NIGHT STEMS','Late Night - Kicks Stem.wav','Late Night - Bass and Sub Stem.wav','Late Night - Drums Stem.wav',199.,128.),
                ('MIDNIGHT PATIENCE STEMS','Kick.wav','Bass and Sub.wav','Drums.wav',116.,28.)]
    for folder,kickfile,bassfile,drumsfile,positive,negative in selections:
        track=next(s for s in registry['sets'] if s['folder']==folder)
        assert track['split']=='development'
        stems=[];hashes={}
        for file in (kickfile,bassfile,drumsfile):
            path=Path(registry['source_root'])/folder/file
            expected=next(f['sha256'] for f in track['files'] if f['file']==f'{folder}/{file}')
            assert sha(path)==expected
            rate,audio=read_audio(path);assert rate==sr
            stems.append(audio);hashes[file]=expected
        kick,bass,drums=stems
        assert len(kick)==len(bass)==len(drums)
        # Common 0.25 gain; even all three unit-range sources cannot clip.
        conditions=[('kick_only',.25*kick),('bass_only',.25*bass),
                    ('kick_bass',.25*(kick+bass)),
                    ('kick_bass_quieter',.25*kick+.0625*bass),
                    ('kick_other_drums',.25*(kick+drums))]
        windows=[dict(id='kick_active_source_passage',start_s=positive,end_s=positive+12),
                 dict(id='kick_absent_source_passage',start_s=negative,end_s=negative+12)]
        for condition,audio in conditions:
            result=probe(audio,sr,windows)
            rows.append(dict(source=folder,condition=condition,source_hashes=hashes,
                             measured_peak=float(np.max(np.abs(audio))),results=result))
            print(folder,condition,{r['variant']:[len(w['fire_times_s']) for w in r['windows']] for r in result},flush=True)
    args.out.write_text(json.dumps(dict(method=__doc__,rows=rows),indent=2)+'\n')

if __name__=='__main__':main()
