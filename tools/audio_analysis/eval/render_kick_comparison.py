"""Local listening pairs: identical full mixes, clicks at actual emitted events."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import soundfile as sf

from .kick_attack_rejection import sha
from .run_kick_dsp_experiments import ROOT, development_sources

STARTS = dict(apricots_128bpm=0,bad_guy_128bpm=0,feel_the_vibration_174bpm=0,
              inhale_exhale_145bpm=0,tears_140bpm=0,late_night=199,
              midnight_patience=116.136375,miracle=84,heavy_on_mind=200)


def render(linear_path,tree_path,audio_root,out):
    reports=[json.loads(p.read_text()) for p in (linear_path,tree_path)]
    variants=[next(v for v in d['variants'] if v['variant']==name)
              for d,name in zip(reports,('linear_15','boosted_depth3'))]
    sources=development_sources(audio_root)
    for row in json.loads((ROOT/'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json').read_text())['tracks']:
        sources.append(dict(track=row['track'],audio_path=row['master_path'],audio_sha256=row['audio_sha256']))
    out.mkdir(parents=True,exist_ok=True);cases=[]
    for source in sources:
        track=source['track'];path=Path(source['audio_path'])
        if sha(path)!=source['audio_sha256']:raise ValueError('source identity changed')
        with sf.SoundFile(path) as audio:
            sr=audio.samplerate;first=round(STARTS[track]*sr);audio.seek(first)
            samples=audio.read(round(8*sr),dtype='float64',always_2d=True)
        start=first/sr;duration=len(samples)/sr
        # Same decaying-tone click family as render_beat_clicks, with no analyzer import.
        t=np.arange(round(.015*sr))/sr
        click=.12*np.sin(2*np.pi*1800*t)*np.exp(-t/.004)
        row=dict(track=track,start_s=start,duration_s=duration,source_sha256=source['audio_sha256'],files=[])
        for name,variant in zip(('A_linear15','B_depth3'),variants):
            result=next(r for r in variant['tracks'] if r['track']==track)
            times=[(h+1)*result['hop']/result['sample_rate'] for h in result['kick_hops']['refined']]
            times=[x for x in times if start<=x<start+duration]
            output=samples*.75
            event_frames=[]
            for emitted in times:
                frame=round((emitted-start)*sr);end=min(len(output),frame+len(click))
                output[frame:end]+=click[:end-frame,None];event_frames.append(frame)
            peak=float(np.max(np.abs(output),initial=0))
            if peak>=1:raise ValueError('comparison would clip; no silent limiter allowed')
            target=out/f'{track}_{name}.wav';sf.write(target,output,sr,subtype='PCM_16')
            # Decode to verify channels, duration and deliberate PCM16 quantisation only.
            decoded,rate=sf.read(target,dtype='float64',always_2d=True)
            if rate!=sr or decoded.shape!=output.shape or np.max(np.abs(decoded-output),initial=0)>1/32768:
                raise ValueError('listening-file verification failed')
            row['files'].append(dict(name=name,path=str(target),sha256=sha(target),sample_rate=sr,
                events_absolute_s=times,event_frames=event_frames,peak=peak,
                maximum_click_alignment_error_ms=max([abs(first+f-x*sr)/sr*1000 for f,x in zip(event_frames,times)],default=0)))
        cases.append(row)
    manifest=dict(purpose=__doc__,selection='First8s of each original clip and first8s of each original kick-heavy master core. Includes improvements and regressions; no cherry-picked event timing.',
        instructions='A is the frozen15-feature linear baseline; B is the depth3 tree. The same stereo mix plays at75% in both. Each short high click marks an emitted detector event. Kicks without clicks are misses; clicks on other sounds are unwanted. Clicks are not beat-grid or truth markers.',
        baseline=dict(matched70=223,extra70=89),candidate=dict(matched70=274,extra70=85),
        limitations='Development examples, provisional labels. Candidate fails per-track retention and kick-free safeguards. No human listening verdict has been claimed. Audio device/display latency is not measured.',
        source_sha256=sha(Path(__file__)),reference_sha256=dict(linear=sha(linear_path),tree=sha(tree_path)),cases=cases)
    (out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    print(json.dumps(dict(cases=len(cases),files=2*len(cases),output=str(out))))
    return manifest


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('linear','tree','audio-root','out'):parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();render(args.linear,args.tree,args.audio_root,args.out)
