#!/usr/bin/env python3
"""Write the compact night scoreboard from the retained night-cache reports."""
from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from tools.audio_analysis.eval.kick_night_common import NIGHT  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / 'tools/audio_analysis/eval/scoreboard/kick_night_2026-10-10.json'


def load(name):
    return json.loads((NIGHT / name).read_text())


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


BAD_GUY = 'bad_guy_128bpm'
# Per-hypothesis cost. CPU is measured in batch Python; state sizes follow from
# the algorithms and are not measured.
COST = dict(
    h19=dict(cpu='glide 0.34% of one core', state='8 band-pass filter values + one evidence window of band-passed audio'),
    h20=dict(cpu='no new audio processing', state='last 8 s of candidate scores (~320 values)'),
    h21=dict(cpu='one extra weighted sum over kernel values the kernel model already computes', state='none new'),
    h22=dict(cpu='H21 + glide 0.34% of one core', state='H19 state + 4 fallback weights'),
    h23=dict(cpu='second feature pass 0.8% of one core', state='frozen extractor buffers + 16 weights'))


def without_bad_guy(per_track):
    rest = [v for k, v in per_track.items() if k != BAD_GUY]
    return [sum(v[0] for v in rest), sum(v[1] for v in rest)]


def frontier_summary():
    f = load('oracle_frontier.json')
    out = {}
    for name, s in f['scorers'].items():
        out[name] = dict(extras_for_90_per_track={t: x['extras_for_90'] for t, x in s['per_track'].items()},
                         **{k: s[k] for k in s if k.startswith('pooled')})
    return out


def costly_summary():
    lab = load('oracle_labels.json')
    tot_costly, tot_extra = {}, {}
    for row in lab.values():
        for k, v in row['costly_classes'].items():
            tot_costly[k] = tot_costly.get(k, 0) + v
        for k, v in row['extras_refined'].items():
            tot_extra[k] = tot_extra.get(k, 0) + v
    return dict(costly_label_classes=tot_costly, extras_at_90_classes=tot_extra,
                per_track={t: dict(oracle=[r['oracle_matched'], r['oracle_extra']], extras_at_90=r['extras_at_90'],
                                   costly=r['costly_classes'], extras=r['extras_refined']) for t, r in lab.items()})


def trial(h):
    t = load(f'{h}/trial.json')
    rows = []
    for v in t['variants']:
        s = v['summary']
        rows.append(dict(name=v.get('name', f"evidence_{v.get('evidence_s')}s"), fast_fires=v.get('fast_fires'),
                         without_bad_guy_70=without_bad_guy(s['per_track']),
                         tol_ms={k: [x['matched'], x['extra']] for k, x in s['tol'].items()},
                         delay_ms=[s['delay_p50'], s['delay_p90'], s['delay_max']],
                         kick_free_core_fires=s['kick_free_extras'], acceptance=v['acceptance'],
                         per_track={k: x[:2] for k, x in s['per_track'].items()},
                         lost_vs_baseline=v.get('lost_vs_baseline')))
    return dict(rule=t['rule'], rule_sha256=t.get('rule_sha256'), configs=len(rows), cost=COST[h], variants=rows)


def main():
    head = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip()
    diag = load('diagnosis.json')
    board = dict(
        status='development research, no candidate promoted; reserved Waypoints / Know You\'re There untouched',
        date='2026-10-10', source_commit=head, labels=381, tolerance_primary_ms=70,
        reference=dict(baseline=[223, 89], h16=[285, 90, '1 core fire'], h18=[262, 89, 'loses Inhale 5.38/6.62']),
        reference_without_bad_guy={k: without_bad_guy(v['per_track']) for k, v in load('replay_summary.json').items()},
        reserved_songs='Waypoints and Know You\'re There audio exists only in Dropbox (EMERGENCE masters and stems); never read by any run, absent from fixtures and caches',
        miss_causes_h18=diag['h18']['miss_causes_total'],
        oracle_per_song_cutoff_diagnostic={k: diag[k]['oracle_total'] for k in diag},
        oracle_frontier=frontier_summary(), oracle_blocking_cases_h22=costly_summary(),
        hypotheses={h: trial(h) for h in ('h19', 'h20', 'h21', 'h22', 'h23')},
        decisions_for_peter=['BUG-7rngq Midnight labels without kick-stem attack', 'BUG-sa3n3 Bad Guy bass-line fires'],
        h22_first_run_void='in-place training-mask edit leaked across folds; archived under h22/void_mask_mutation_run',
        validation=load('validation.json'), extra_passages=load('extra_passages.json'),
        relabel=load('relabel_summary.json'), glide_cpu=load('glide_meta.json'),
        snapped_label_sha256=sha(NIGHT / 'snapped_labels_frozen.json'),
        cache=str(NIGHT))
    OUT.write_text(json.dumps(board, indent=1, default=str) + '\n')
    print(OUT, sha(OUT))


if __name__ == '__main__':
    main()
