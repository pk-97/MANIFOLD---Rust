#!/usr/bin/env python3
"""Runs a plan of lab steps strictly one after another, each through the GPU lock (scripts/gpu_queue.py), so a night
of experiments needs no one at the keyboard. Nothing clever: no retries, no judgment, no config edits. The first step
that exits non-zero ends the queue; the steps after it stay unrun and the summary says so.

One exception, the paging watchdog: every 5 seconds the machine's pageout counter (vm_stat on the Mac, /proc/vmstat
on Linux) is read; if it has risen
at every sample for --swap-watch-seconds (default 60, 0 off) the machine is paging for real, so the step's process
group and every descendant are killed, the step is recorded as exit 137 and the queue goes on to the next step. A
healthy run can hold 20+ GB of clean memory-mapped spectra without paging, so resident size is not the trigger; it is
only recorded. Each summary row carries the step's peak resident memory, the pageouts during it and the swap used at
its end. The queue still ends with exit 137 after a kill so the morning sees it. Every other failure stops the queue
as above.

The plan is JSON:
  {"name": "tuesday", "steps": [
     {"run": "snare_base"},
     {"run": "snare_gain", "args": ["--seeds", "0,1"]},
     {"compare": ["snare_base", "snare_gain"], "release": true},
     {"release": "snare_gain", "args": ["--seed", "0", "--nets", "3"]}]}
A step is a run (lab/run.py CONFIG), a compare (lab/compare.py A B) or a release (lab/release.py train CONFIG; it
needs the run's seeds SEED..SEED+NETS-1 already trained). "args" is appended verbatim. On a run or compare step,
"release" is passed through as --release (true), or --release VALUE (a string); a step with "release" and no
"run"/"compare" is a release step and its value is the config name. Each step runs as
  scripts/gpu_queue.py --label ... -- python lab/<run|compare|release>.py ...
Output goes to ~/.cache/manifold/detector-lab/queue/<name>_<stamp>/: one NN_<kind>_<config>.log per step and
summary.txt (start, end, wall seconds, exit code and the compare headline lines per step), rewritten after every
step so a crash leaves the record up to there.

Named overnight.py, not queue.py: a script named queue.py in the lab folder shadows the standard library's queue for
run.py (torch imports it) whenever the lab folder leads sys.path.

Usage: overnight.py PLAN.json [--dry-run] [--out DIR] [--swap-watch-seconds N]"""
import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
LAB = ROOT / 'tools/audio_analysis/lab'
GPU_QUEUE = ROOT / 'scripts/gpu_queue.py'
OUT = Path.home() / '.cache/manifold/detector-lab/queue'
SCRIPT = {'run': 'run.py', 'compare': 'compare.py', 'release': 'release.py'}
SWAP_WATCH_S = 60   # sustained paging this long kills the step (module doc); 0 turns the watch off
POLL_S = 5
STEP_KEYS = {'run', 'compare', 'args', 'release'}
HEADLINE = re.compile(r'^(old exam \| )?(B - A over (songs|seeds)|[AB]: F1 |songs better / worse|  [AB]: coincident '
                      r'|old exam \()')


def load_plan(path):
    """(name, steps) of a plan file; SystemExit with the reason when it is not a plan."""
    d = json.loads(Path(path).read_text())
    if not isinstance(d, dict) or not isinstance(d.get('steps'), list) or not d['steps']:
        sys.exit(f'{path}: a plan is an object with a non-empty "steps" list')
    for i, s in enumerate(d['steps'], 1):
        if not isinstance(s, dict) or set(s) - STEP_KEYS:
            sys.exit(f'{path}: step {i}: keys must be among {sorted(STEP_KEYS)}')
        kinds = [k for k in ('run', 'compare') if k in s] or [k for k in ('release',) if k in s]
        if len(kinds) != 1:
            sys.exit(f'{path}: step {i}: exactly one of "run", "compare" or (alone) "release"')
        want = [str, list][kinds[0] == 'compare']
        v = s[kinds[0]]
        if not isinstance(v, want) or (want is list and (len(v) != 2 or not all(isinstance(x, str) for x in v))):
            sys.exit(f'{path}: step {i}: "run" and "release" are config names, "compare" is [A, B]')
        if not isinstance(s.get('args', []), list) or not all(isinstance(x, str) for x in s.get('args', [])):
            sys.exit(f'{path}: step {i}: "args" is a list of strings')
        if kinds[0] != 'release' and not isinstance(s.get('release', False), (bool, str)):
            sys.exit(f'{path}: step {i}: "release" is true or a string')
    return d.get('name') or Path(path).stem, d['steps']


def kind_of(step):
    return 'run' if 'run' in step else 'compare' if 'compare' in step else 'release'


def what_of(step):
    v = step[kind_of(step)]
    return v if isinstance(v, str) else ' '.join(v)


def step_command(step, n):
    """The argv of one step: the GPU lock around the lab script."""
    kind = kind_of(step)
    configs = list(step[kind]) if kind == 'compare' else [step[kind]]
    cmd = [sys.executable, str(LAB / SCRIPT[kind]), *(['train'] if kind == 'release' else []), *configs,
           *step.get('args', [])]
    rel = step.get('release', False)
    if rel and kind != 'release':
        cmd += ['--release'] + ([rel] if isinstance(rel, str) else [])
    label = f'lab-queue step {n}: {kind} {" ".join(configs)}'
    return [sys.executable, str(GPU_QUEUE), '--label', label, '--'] + cmd


def log_name(step, n):
    kind = kind_of(step)
    configs = step[kind] if kind == 'compare' else [step[kind]]
    return f'{n:02d}_{kind}_{"_vs_".join(Path(c).stem for c in configs)}.log'


def headlines(log):
    """The compare headline lines in a step's log."""
    return [ln.rstrip() for ln in Path(log).read_text(errors='replace').splitlines() if HEADLINE.match(ln)]


def stamp(t):
    return time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(t))


def tree_rss(root):
    """(resident KB of root and all its descendants, their pids), from one ps listing; (0, []) when root is gone."""
    out = subprocess.run(['ps', '-eo', 'pid=,ppid=,rss='], capture_output=True, text=True).stdout
    kids, rss = {}, {}
    for ln in out.splitlines():
        f = ln.split()
        if len(f) == 3 and all(x.isdigit() for x in f):
            pid, ppid, kb = map(int, f)
            kids.setdefault(ppid, []).append(pid)
            rss[pid] = kb
    pids, todo = [], [root]
    while todo:
        p = todo.pop()
        if p in rss:
            pids.append(p)
        todo += kids.get(p, [])
    return sum(rss[p] for p in pids), pids


def vm_sample(system=sys.platform, read=lambda p: Path(p).read_text()):
    """(pageouts since boot, swap used in MB). The one place the machine is read, so preflight and the watchdog see
    the same numbers. The Mac: vm_stat's Pageouts counter and sysctl vm.swapusage. Linux: /proc/vmstat's pswpout
    (pages written to swap) and SwapTotal - SwapFree from /proc/meminfo."""
    if system == 'linux':
        vm, mem = read('/proc/vmstat'), read('/proc/meminfo')
        po = re.search(r'^pswpout (\d+)', vm, re.M)
        kb = [re.search(rf'^{k}:\s+(\d+) kB', mem, re.M) for k in ('SwapTotal', 'SwapFree')]
        if not po or not all(kb):
            raise RuntimeError(f'cannot read paging counters: /proc/vmstat gave {vm[:80]!r}, /proc/meminfo gave {mem[:80]!r}')
        return int(po.group(1)), (int(kb[0].group(1)) - int(kb[1].group(1))) / 1024
    vm = subprocess.run(['vm_stat'], capture_output=True, text=True).stdout
    swap = subprocess.run(['sysctl', '-n', 'vm.swapusage'], capture_output=True, text=True).stdout
    po = re.search(r'^Pageouts:\s+(\d+)', vm, re.M)
    used = re.search(r'used = ([\d.]+)M', swap)
    if not po or not used:
        raise RuntimeError(f'cannot read paging counters: vm_stat gave {vm[:80]!r}, vm.swapusage gave {swap[:80]!r}')
    return int(po.group(1)), float(used.group(1))


class PagingWatch:
    """True from update() once the pageout counter has risen at every sample for watch_s seconds straight. One
    sample that does not rise (or a counter that goes back) restarts the count; watch_s 0 never trips."""

    def __init__(self, watch_s, pageouts, now):
        self.watch_s, self.last, self.rising_since = watch_s, pageouts, None
        self.t = now

    def update(self, pageouts, now):
        if pageouts > self.last:
            if self.rising_since is None:
                self.rising_since = self.t
        else:
            self.rising_since = None
        self.last, self.t = pageouts, now
        return bool(self.watch_s) and self.rising_since is not None and now - self.rising_since >= self.watch_s


def execute(cmd, log, watch_s=SWAP_WATCH_S, poll_s=POLL_S, vm=vm_sample, rss=tree_rss, clock=time.monotonic):
    """Runs cmd with its output in log, in its own process group, sampling the machine every poll_s seconds. If the
    pageout counter rises at every sample for watch_s seconds (sustained paging) the group and every descendant are
    killed (SIGKILL). Returns dict(code, killed, peak_gb, pageouts, swap_mb): the step's peak resident memory, the
    pageouts during it and the swap used at the end; code is 137 when killed."""
    po0, swap = vm()
    watch = PagingWatch(watch_s, po0, clock())
    peak_kb, po = 0, po0
    with open(log, 'w') as f:
        proc = subprocess.Popen(cmd, stdout=f, stderr=subprocess.STDOUT, start_new_session=True)
        killed = False
        while True:
            try:
                rc = proc.wait(timeout=poll_s)
                break
            except subprocess.TimeoutExpired:
                pass
            kb, pids = rss(proc.pid)
            peak_kb = max(peak_kb, kb)
            po, swap = vm()
            if watch.update(po, clock()):
                for p in pids:
                    try:
                        os.kill(p, signal.SIGKILL)
                    except OSError:
                        pass
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except OSError:
                    pass
                proc.wait()
                rc, killed = 137, True
                f.write(f'\n[overnight.py] killed: pageouts rose at every sample for {watch_s:g} s '
                        f'(+{po - po0}, swap used {swap:.0f} MB)\n')
                break
    po, swap = vm()
    return dict(code=rc, killed=killed, peak_gb=peak_kb / 1e6, pageouts=po - po0, swap_mb=swap)


def as_result(r):
    """A runner's result as a dict: an int is a bare exit code."""
    return dict(code=r, killed=False) if isinstance(r, int) else r


def summary_text(name, started, rows, steps, finished, stopped=None):
    out = [f'queue {name}: started {stamp(started)}' + (f', finished {stamp(finished)}' if finished else ''),
           f'{len(steps)} steps']
    for r in rows:
        why = f' (KILLED: sustained paging, pageouts rose at every sample for {r["watch_s"]:g} s)' if r['killed'] else ''
        peak = (f', peak {r["peak_gb"]:.1f} GB resident, pageouts +{r["pageouts"]}, swap used {r["swap_mb"]:.0f} MB'
                if r.get('peak_gb') is not None else '')
        out.append(f'step {r["n"]} {r["kind"]} {r["what"]}: exit {r["code"]}{why}{peak}, {stamp(r["start"])} to '
                   f'{stamp(r["end"])}, {r["end"] - r["start"]:.0f} s, log {r["log"]}')
        out += [f'    {ln}' for ln in r['headlines']]
    if stopped is not None:
        left = list(range(len(rows) + 1, len(steps) + 1))
        out.append(f'STOPPED: step {stopped["n"]} exited {stopped["code"]}; not run: '
                   + (', '.join(f'step {n} ({kind_of(steps[n - 1])} {what_of(steps[n - 1])})' for n in left)
                      or 'none'))
    elif finished:
        killed = [r['n'] for r in rows if r['killed']]
        out.append(f'all {len(steps)} steps ran; killed for sustained paging: '
                   + ', '.join(f'step {n} ({rows[n - 1]["kind"]} {rows[n - 1]["what"]})' for n in killed)
                   if killed else f'all {len(steps)} steps passed')
    return '\n'.join(out) + '\n'


def run_plan(name, steps, out_dir, runner=execute, clock=time.time, log=print, watch_s=SWAP_WATCH_S):
    """Runs the steps in order through runner(cmd, log_path) -> exit code, or the dict execute() returns;
    stops at the first non-zero exit unless the runner killed the step, which is recorded and passed.
    Writes the logs and summary.txt into out_dir. Returns the exit code: the failing step's, else 137 if any step
    was killed, else 0."""
    out_dir.mkdir(parents=True, exist_ok=True)
    started, rows, stopped, code, killed_any = clock(), [], None, 0, False
    for n, step in enumerate(steps, 1):
        kind, what = kind_of(step), what_of(step)
        path = out_dir / log_name(step, n)
        log(f'[{stamp(clock())}] step {n}/{len(steps)}: {kind} {what}')
        t0 = clock()
        try:
            res = as_result(runner(step_command(step, n), path))
        except KeyboardInterrupt:
            res = dict(code=130, killed=False)
        rc, killed = res['code'], res['killed']
        if not path.exists():
            path.write_text('')
        row = dict(n=n, kind=kind, what=what, code=rc, killed=killed, watch_s=watch_s, peak_gb=res.get('peak_gb'),
                   pageouts=res.get('pageouts'), swap_mb=res.get('swap_mb'), start=t0, end=clock(),
                   log=path.name, headlines=headlines(path) if kind == 'compare' else [])
        rows.append(row)
        killed_any = killed_any or killed
        if rc and not killed:
            stopped, code = row, rc
        (out_dir / 'summary.txt').write_text(summary_text(name, started, rows, steps, None, stopped))
        if stopped:
            break
    finished = clock()
    (out_dir / 'summary.txt').write_text(summary_text(name, started, rows, steps, finished, stopped))
    log(f'-> {out_dir / "summary.txt"}')
    return code or (137 if killed_any else 0)


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
    watch = float(a[a.index('--swap-watch-seconds') + 1]) if '--swap-watch-seconds' in a else SWAP_WATCH_S
    return run_plan(name, steps, base, runner=lambda cmd, log: execute(cmd, log, watch), watch_s=watch)


if __name__ == '__main__':
    sys.exit(main())
