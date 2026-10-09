"""Evaluate frozen v5/persistence detectors after detector-blind heldout review."""
from __future__ import annotations
import argparse
import json
from pathlib import Path
from .kick_attack_rejection import read_audio, causal_features, sha
from .kick_dsp_experiments import detect_v5
from .kick_sustain_audit import validate_confirmed_fires
from .master_kick_comparison import score_passage
from .run_kick_dsp_experiments import ROOT


def totals(passages):
    return dict(tolerance_ms={str(ms):{k:sum(p['accuracy_by_tolerance_ms'][str(ms)][k]
        for p in passages) for k in ('matched','missed','extra')} for ms in (35,50,70)},
        association={k:sum(p['association_early_35_late_200_ms'][k] for p in passages)
        for k in ('matched','missed','extra')})


def run(protocol_path, labels_path, out):
    protocol=json.loads(protocol_path.read_text())
    labels=json.loads(labels_path.read_text())
    if labels['protocol_sha256'] != sha(protocol_path):
        raise ValueError('labels do not identify the frozen protocol')
    if set(t['track'] for t in labels['tracks']) != set(protocol['tracks']):
        raise ValueError('heldout track set differs from protocol')
    for name,expected in protocol['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name) != expected:
            raise ValueError(f'frozen implementation changed: {name}')
    tracks=[]
    for track in labels['tracks']:
        path=Path(track['master_path'])
        if sha(path) != track['audio_sha256']:
            raise ValueError(f'audio differs from reviewed file: {path}')
        sr,audio=read_audio(path)
        env,hop=causal_features(audio,sr)
        baseline=detect_v5(env,sr,hop)
        candidate,decisions=validate_confirmed_fires(env,baseline,sr,hop)
        scores={name:[dict(id=p['id'],kick_free=p['kick_free'],
            **score_passage([(i+1)*hop/sr for i in fires],p)) for p in track['passages']]
            for name,fires in [('v5',baseline),('persistence',candidate)]}
        comparisons=[]
        for before,after in zip(scores['v5'],scores['persistence']):
            a=before['association_early_35_late_200_ms']
            b=after['association_early_35_late_200_ms']
            old={p['attack_s']:p for p in a['pairs']}
            new={p['attack_s']:p for p in b['pairs']}
            comparisons.append(dict(passage=before['id'],kick_free=before['kick_free'],
                lost_associated_labels_s=sorted(old.keys()-new.keys()),
                new_associated_labels_s=sorted(new.keys()-old.keys()),
                unmatched_before=a['extra'],unmatched_after=b['extra'],
                changed_timing=[dict(attack_s=t,baseline_available_s=old[t]['available_s'],
                    candidate_available_s=new[t]['available_s']) for t in old.keys()&new.keys()
                    if old[t]['available_s']!=new[t]['available_s']]))
        waits=[(d['accepted_hop']-d['original_hop'])*hop/sr for d in decisions
               if d['accepted_hop'] is not None]
        if any(w<0 or w>.035+1e-12 for w in waits):
            raise ValueError('candidate timing violates frozen validation budget')
        tracks.append(dict(track=track['track'],audio_sha256=track['audio_sha256'],
            sample_rate=sr,hop=hop,duration_s=len(audio)/sr,baseline_hops=baseline,
            candidate_hops=candidate,decisions=decisions,scores=scores,
            comparisons=comparisons,max_extra_wait_ms=max(waits,default=0)*1000,
            output_gaps_under_60ms=[dict(available_s=(b+1)*hop/sr,gap_ms=(b-a)*hop/sr*1000)
                for a,b in zip(candidate,candidate[1:]) if (b-a)*hop/sr<.06-1e-12]))
        print(track['track'],{name:totals(s) for name,s in scores.items()},flush=True)
    overall={name:totals([p for t in tracks for p in t['scores'][name]])
             for name in ('v5','persistence')}
    comparisons=[p for t in tracks for p in t['comparisons']]
    criteria=dict(all_baseline_associated_labels_retained=not any(
        p['lost_associated_labels_s'] for p in comparisons),
        fewer_unmatched_overall=overall['persistence']['association']['extra']<overall['v5']['association']['extra'],
        no_increase_on_either_track=all(totals(t['scores']['persistence'])['association']['extra']<=
            totals(t['scores']['v5'])['association']['extra'] for t in tracks),
        no_increase_on_kick_free_passages=all(p['unmatched_after']<=p['unmatched_before']
            for p in comparisons if p['kick_free']),extra_wait_within_35ms=True)
    report=dict(protocol_sha256=sha(protocol_path),labels_sha256=sha(labels_path),
        runner_sha256=sha(Path(__file__)),frozen_source_sha256=protocol['source_sha256'],
        totals=overall,criteria=criteria,passes_all_frozen_criteria=all(criteria.values()),
        tracks=tracks,limitations='Two heldout recordings with short stem-assisted visually reviewed passages. '
        'Provisional timing, not audited listening truth or native callback/display latency. '
        'Full-master mono, chronological causal processing. No tuning or parameter changes.')
    out.parent.mkdir(parents=True,exist_ok=True)
    out.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(criteria))
    return report


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--protocol',type=Path,required=True)
    ap.add_argument('--labels',type=Path,required=True)
    ap.add_argument('--out',type=Path,required=True)
    args=ap.parse_args()
    run(args.protocol,args.labels,args.out)

if __name__=='__main__':main()
