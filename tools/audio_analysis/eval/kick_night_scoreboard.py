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


def trial(h):
    t = load(f'{h}/trial.json')
    rows = []
    for v in t['variants']:
        s = v['summary']
        rows.append(dict(name=v.get('name', f"evidence_{v.get('evidence_s')}s"), fast_fires=v.get('fast_fires'),
                         tol_ms={k: [x['matched'], x['extra']] for k, x in s['tol'].items()},
                         delay_ms=[s['delay_p50'], s['delay_p90'], s['delay_max']],
                         kick_free_core_fires=s['kick_free_extras'], acceptance=v['acceptance'],
                         per_track={k: x[:2] for k, x in s['per_track'].items()},
                         lost_vs_baseline=v.get('lost_vs_baseline')))
    return dict(rule=t['rule'], rule_sha256=t.get('rule_sha256'), variants=rows)


def main():
    head = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip()
    diag = load('diagnosis.json')
    board = dict(
        status='development research, no candidate promoted; reserved Waypoints / Know You\'re There untouched',
        date='2026-10-10', source_commit=head, labels=381, tolerance_primary_ms=70,
        reference=dict(baseline=[223, 89], h16=[285, 90, '1 core fire'], h18=[262, 89, 'loses Inhale 5.38/6.62']),
        miss_causes_h18=diag['h18']['miss_causes_total'],
        oracle_per_song_cutoff_diagnostic={k: diag[k]['oracle_total'] for k in diag},
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
