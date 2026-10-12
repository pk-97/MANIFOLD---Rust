#!/usr/bin/env python3
"""Runs a plan of lab steps strictly one after another, each through the GPU lock (scripts/gpu_queue.py), so a night
of experiments needs no one at the keyboard. Nothing clever: no retries, no judgment, no config edits. The first step
that exits non-zero ends the queue; the steps after it stay unrun and the summary says so.

The plan is JSON:
  {"name": "tuesday", "steps": [
     {"run": "snare_base"},
     {"run": "snare_gain", "args": ["--seeds", "0,1"]},
     {"compare": ["snare_base", "snare_gain"], "release": true}]}
A step is a run (lab/run.py CONFIG) or a compare (lab/compare.py A B). "args" is appended verbatim. "release" is
passed through as --release (true), or --release VALUE (a string). Each step runs as
  scripts/gpu_queue.py --label ... -- python lab/<run|compare>.py ...
Output goes to ~/.cache/manifold/detector-lab/queue/<name>_<stamp>/: one NN_<kind>_<config>.log per step and
summary.txt (start, end, wall seconds, exit code and the compare headline lines per step), rewritten after every
step so a crash leaves the record up to there.

Named overnight.py, not queue.py: a script named queue.py in the lab folder shadows the standard library's queue for
run.py (torch imports it) whenever the lab folder leads sys.path.

Usage: overnight.py PLAN.json [--dry-run] [--out DIR]"""
import json
import re
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
LAB = ROOT / 'tools/audio_analysis/lab'
GPU_QUEUE = ROOT / 'scripts/gpu_queue.py'
OUT = Path.home() / '.cache/manifold/detector-lab/queue'
SCRIPT = {'run': 'run.py', 'compare': 'compare.py'}
STEP_KEYS = {'run', 'compare', 'args', 'release'}
HEADLINE = re.compile(r'^(B - A over (songs|seeds)|[AB]: F1 |songs better / worse|  [AB]: coincident )')


def load_plan(path):
    """(name, steps) of a plan file; SystemExit with the reason when it is not a plan."""
    d = json.loads(Path(path).read_text())
    if not isinstance(d, dict) or not isinstance(d.get('steps'), list) or not d['steps']:
        sys.exit(f'{path}: a plan is an object with a non-empty "steps" list')
    for i, s in enumerate(d['steps'], 1):
        if not isinstance(s, dict) or set(s) - STEP_KEYS:
            sys.exit(f'{path}: step {i}: keys must be among {sorted(STEP_KEYS)}')
        kinds = [k for k in SCRIPT if k in s]
        if len(kinds) != 1:
            sys.exit(f'{path}: step {i}: exactly one of "run" or "compare"')
        want = [str, list][kinds[0] == 'compare']
        v = s[kinds[0]]
        if not isinstance(v, want) or (want is list and (len(v) != 2 or not all(isinstance(x, str) for x in v))):
            sys.exit(f'{path}: step {i}: "run" is a config name, "compare" is [A, B]')
        if not isinstance(s.get('args', []), list) or not all(isinstance(x, str) for x in s.get('args', [])):
            sys.exit(f'{path}: step {i}: "args" is a list of strings')
        if not isinstance(s.get('release', False), (bool, str)):
            sys.exit(f'{path}: step {i}: "release" is true or a string')
    return d.get('name') or Path(path).stem, d['steps']


def kind_of(step):
    return 'run' if 'run' in step else 'compare'


def what_of(step):
    v = step[kind_of(step)]
    return v if isinstance(v, str) else ' '.join(v)


def step_command(step, n):
    """The argv of one step: the GPU lock around the lab script."""
    kind = kind_of(step)
    configs = [step[kind]] if kind == 'run' else list(step[kind])
    cmd = [sys.executable, str(LAB / SCRIPT[kind]), *configs, *step.get('args', [])]
    rel = step.get('release', False)
    if rel:
        cmd += ['--release'] + ([rel] if isinstance(rel, str) else [])
    label = f'lab-queue step {n}: {kind} {" ".join(configs)}'
    return [sys.executable, str(GPU_QUEUE), '--label', label, '--'] + cmd


def log_name(step, n):
    kind = kind_of(step)
    configs = [step[kind]] if kind == 'run' else step[kind]
    return f'{n:02d}_{kind}_{"_vs_".join(Path(c).stem for c in configs)}.log'


def headlines(log):
    """The compare headline lines in a step's log."""
    return [ln.rstrip() for ln in Path(log).read_text(errors='replace').splitlines() if HEADLINE.match(ln)]


def stamp(t):
    return time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(t))


def execute(cmd, log):
    """Runs cmd with its output in log; returns its exit code."""
    with open(log, 'w') as f:
        return subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT).returncode


def summary_text(name, started, rows, steps, finished, stopped=None):
    out = [f'queue {name}: started {stamp(started)}' + (f', finished {stamp(finished)}' if finished else ''),
           f'{len(steps)} steps']
    for r in rows:
        out.append(f'step {r["n"]} {r["kind"]} {r["what"]}: exit {r["code"]}, {stamp(r["start"])} to {stamp(r["end"])}, '
                   f'{r["end"] - r["start"]:.0f} s, log {r["log"]}')
        out += [f'    {ln}' for ln in r['headlines']]
    if stopped is not None:
        left = list(range(len(rows) + 1, len(steps) + 1))
        out.append(f'STOPPED: step {stopped["n"]} exited {stopped["code"]}; not run: '
                   + (', '.join(f'step {n} ({kind_of(steps[n - 1])} {what_of(steps[n - 1])})' for n in left)
                      or 'none'))
    elif finished:
        out.append(f'all {len(steps)} steps passed')
    return '\n'.join(out) + '\n'


def run_plan(name, steps, out_dir, runner=execute, clock=time.time, log=print):
    """Runs the steps in order through runner(cmd, log_path) -> exit code; stops at the first non-zero. Writes the
    logs and summary.txt into out_dir. Returns the exit code (0, or the failing step's)."""
    out_dir.mkdir(parents=True, exist_ok=True)
    started, rows, stopped, code = clock(), [], None, 0
    for n, step in enumerate(steps, 1):
        kind, what = kind_of(step), what_of(step)
        path = out_dir / log_name(step, n)
        log(f'[{stamp(clock())}] step {n}/{len(steps)}: {kind} {what}')
        t0 = clock()
        try:
            rc = runner(step_command(step, n), path)
        except KeyboardInterrupt:
            rc = 130
        if not path.exists():
            path.write_text('')
        row = dict(n=n, kind=kind, what=what, code=rc, start=t0, end=clock(), log=path.name,
                   headlines=headlines(path) if kind == 'compare' else [])
        rows.append(row)
        if rc:
            stopped, code = row, rc
        (out_dir / 'summary.txt').write_text(summary_text(name, started, rows, steps, None, stopped))
        if rc:
            break
    finished = clock()
    (out_dir / 'summary.txt').write_text(summary_text(name, started, rows, steps, finished, stopped))
    log(f'-> {out_dir / "summary.txt"}')
    return code


def main(argv=None):
    a = sys.argv[1:] if argv is None else argv
    if not a or a[0].startswith('--'):
        sys.exit(__doc__)
    name, steps = load_plan(a[0])
    if '--dry-run' in a:
        for n, s in enumerate(steps, 1):
            print(' '.join(step_command(s, n)))
        return 0
    base = Path(a[a.index('--out') + 1]) if '--out' in a else OUT / f'{name}_{time.strftime("%Y%m%d_%H%M%S")}'
    return run_plan(name, steps, base)


if __name__ == '__main__':
    sys.exit(main())
