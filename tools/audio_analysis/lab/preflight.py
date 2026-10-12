#!/usr/bin/env python3
"""Read-only check that an overnight plan (lab/overnight.py) can run. One line per check, PASS or FAIL with the
reason; a failed check lists what is missing under it; exit 1 if any failed. It trains nothing, builds nothing and
writes nothing.

  1  tools/audio_analysis has no uncommitted change (release.py refuses a dirty tree)
  2  torch reports MPS here (run.py refuses to train a net on the CPU)
  3  the home volume has at least 50 GB free
  4  every config in a run, compare or release step resolves: run.py --plan prints its run id
  5  every snare-family config: its exam and holdout songs are in the kick verdict store (lab/kick_verdict.py)
  6  every --exam-of RUN_ID has a meta.json with an exam list
  7  caches are warm: every artifact run.py --warm would build (spectrum, candidates, frame and event measures,
     patches) already exists for every song the config trains or scores; a miss names the song and artifact
  8  the GPU lock (scripts/gpu_queue.py) is free, or its holder is a live process (reported either way)
  9  the plan passes overnight.py load_plan

Check 7 runs run.py's own warm() with cache.artifact swapped for a lookup that records a miss and returns a stub, so
its list of what a seed reads is the one that really runs. Each config is inspected in its own process (the net's
env is read at import, so two families cannot share one).

Usage: preflight.py PLAN.json"""
import contextlib
import io
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
LAB = ROOT / 'tools/audio_analysis/lab'
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / 'scripts'))

MIN_FREE_GB = 50
SHOWN = 12   # misses listed under a failed check


@contextlib.contextmanager
def peeking(misses):
    """cache.artifact (and the name components.py bound at import) replaced by a lookup: a hit returns a stub array,
    a miss appends (song id, artifact name) to misses and returns the stub too. Nothing is built or written."""
    import numpy as np
    from tools.audio_analysis.eval.bundles import BundleStamp, _filename_stem_for
    from tools.audio_analysis.lab import cache, components

    def peek(song, name, settings, build, root=cache.ROOT / 'artifacts', file_backed=False):
        stamp = BundleStamp(pipeline_version=f'{cache.VERSION}/{name}',
                            models={k: str(v) for k, v in sorted({**settings, 'loader': cache.loader_id()}.items())})
        stem = _filename_stem_for(song.content_id, stamp)
        sidecar = file_backed and (root / f'{stem}.npy').exists()
        if not (sidecar or ((root / f'{stem}.json').exists() and (root / f'{stem}.npz').exists())):
            misses.append((song.id, name))
        return np.zeros((2, 1), np.float32)
    saved = cache.artifact, components.artifact
    cache.artifact = components.artifact = peek
    try:
        yield
    finally:
        cache.artifact, components.artifact = saved


def inspect_config(name, release=False):
    """In this process: the config's family, its exam and holdout songs' content ids, and the cache misses of every
    song it reads. The child side of check 4-7; run in a fresh process per config."""
    from tools.audio_analysis.lab import run
    from tools.audio_analysis.lab.config import load
    cfg = load(name)
    _, songs, meta = run.setup(cfg, release)
    misses = []
    with peeking(misses), contextlib.redirect_stdout(io.StringIO()):
        run.warm(cfg)
    names = sorted(set(meta['exam']) | set(meta['holdout']))
    return dict(family=cfg.family, songs={t: songs[t].content_id for t in names}, misses=sorted(set(misses)))


class Probe:
    """Everything that touches the machine; the tests replace it with fakes."""
    runs = Path.home() / '.cache/manifold/detector-lab/runs'

    def git_dirty(self):
        r = subprocess.run(['git', '-C', str(ROOT), 'status', '--porcelain', '--', 'tools/audio_analysis'],
                           capture_output=True, text=True)
        if r.returncode:
            raise RuntimeError(r.stderr.strip())
        return r.stdout.splitlines()

    def mps(self):
        """(available, why not)."""
        if os.environ.get('KICK_GOAL_NN_DEVICE') == 'cpu':
            return False, 'KICK_GOAL_NN_DEVICE=cpu is set in the environment'
        import torch
        if torch.backends.mps.is_available():
            return True, ''
        if not torch.backends.mps.is_built():
            return False, 'this torch build has no MPS support'
        return False, 'torch reports MPS unavailable in this process (no Metal GPU visible here)'

    def free_bytes(self):
        return shutil.disk_usage(Path.home()).free

    def run_id(self, config, release):
        """(ok, run id or the reason): what run.py --plan prints, in its own process like overnight's run_id_of."""
        r = subprocess.run([sys.executable, str(LAB / 'run.py'), '--plan', config, *(['--release'] if release else [])],
                           capture_output=True, text=True)
        out = r.stdout.strip().splitlines()
        if r.returncode:
            tail = (r.stderr.strip() or r.stdout.strip()).splitlines()
            return False, tail[-1] if tail else f'exit {r.returncode}'
        return True, out[-1]

    def inspect(self, config, release):
        r = subprocess.run([sys.executable, str(Path(__file__)), '--inspect', config, *(['--release'] if release else [])],
                           capture_output=True, text=True)
        if r.returncode:
            tail = (r.stderr.strip() or r.stdout.strip()).splitlines()
            raise RuntimeError(tail[-1] if tail else f'exit {r.returncode}')
        return json.loads(r.stdout.strip().splitlines()[-1])

    def verdict_store(self):
        """(path, set of keys in the kick verdict store of the shipped kick release)."""
        import numpy as np
        from tools.audio_analysis.lab import kick_verdict
        path = kick_verdict.store_path(kick_verdict.kick_release())
        if not path.exists():
            raise FileNotFoundError(f'no kick verdict store {path.name}: run lab/kick_verdict.py build')
        with np.load(path) as z:
            return path, set(z.files)

    def gpu(self):
        """(free, holder text, holder alive)."""
        import gpu_queue
        out = io.StringIO()
        rc = gpu_queue.status(out=out)
        if rc == 0:
            return True, out.getvalue().strip(), True
        holder = gpu_queue.read_holder(gpu_queue.queue_dir())
        pid = holder.get('pid', '')
        alive = False
        if pid.isdigit() and int(pid) > 1:
            try:
                os.kill(int(pid), 0)
                alive = True
            except PermissionError:
                alive = True
            except OSError:
                pass
        return False, out.getvalue().strip(), alive


def configs_of(steps):
    """[(config, release)] in plan order, each once, and the --exam-of run ids in plan order, each once."""
    cfgs, exams = [], []
    for s in steps:
        kind = 'run' if 'run' in s else 'compare' if 'compare' in s else 'release'
        names = list(s[kind]) if kind == 'compare' else [s[kind]]
        args = s.get('args', [])
        rel = kind != 'release' and (bool(s.get('release')) or '--release' in args)
        cfgs += [(n, rel) for n in names if (n, rel) not in cfgs]
        for i, a in enumerate(args[:-1]):
            if a == '--exam-of' and args[i + 1] not in exams:
                exams.append(args[i + 1])
    return cfgs, exams


def listed(items, n=SHOWN):
    items = list(items)
    return items[:n] + ([f'... and {len(items) - n} more'] if len(items) > n else [])


def check(plan_path, probe, load_plan):
    """[(ok, title, summary, detail lines)] for checks 1-9 in order."""
    rows = []

    def add(ok, title, summary, detail=()):
        rows.append((ok, title, summary, list(detail)))

    try:
        dirty = probe.git_dirty()
        add(not dirty, 'tools/audio_analysis committed',
            'clean' if not dirty else f'{len(dirty)} uncommitted path(s); release.py refuses a dirty tree',
            listed(dirty) if dirty else ())
    except Exception as e:
        add(False, 'tools/audio_analysis committed', f'git status failed: {e}')

    try:
        ok, why = probe.mps()
        add(ok, 'MPS available', 'torch reports MPS' if ok else why)
    except Exception as e:
        add(False, 'MPS available', f'torch import failed: {e}')

    free = probe.free_bytes() / 1e9
    add(free >= MIN_FREE_GB, 'free disk', f'{free:.0f} GB free on the home volume'
        + ('' if free >= MIN_FREE_GB else f', need {MIN_FREE_GB}'))

    try:
        plan_name, steps = load_plan(plan_path)
        plan_err = None
    except SystemExit as e:
        plan_name, steps, plan_err = None, [], str(e.code)
    except Exception as e:
        plan_name, steps, plan_err = None, [], f'{type(e).__name__}: {e}'
    cfgs, exams = configs_of(steps)

    ids, bad, resolved = {}, [], []
    for c, rel in cfgs:
        ok, text = probe.run_id(c, rel)
        ids[(c, rel)] = text
        if ok:
            resolved.append((c, rel))
        else:
            bad.append(f'{c}{" --release" if rel else ""}: {text}')
    if plan_err:
        add(False, 'configs resolve', f'no plan to read ({plan_err})')
    else:
        add(not bad, 'configs resolve',
            ', '.join(f'{c}{" --release" if rel else ""} -> {ids[(c, rel)]}' for c, rel in cfgs) if not bad
            else f'{len(bad)} of {len(cfgs)} configs fail run.py --plan', bad)

    seen, inspected = {}, {}
    for key in resolved:
        try:
            seen[key] = probe.inspect(*key)
        except Exception as e:
            inspected[key] = str(e)

    snares = [k for k, v in seen.items() if v['family'] == 'snare']
    if plan_err:
        add(False, 'kick verdicts for snare songs', f'no plan to read ({plan_err})')
    elif not snares:
        add(not inspected, 'kick verdicts for snare songs',
            'no snare-family config in the plan' if not inspected else 'cannot tell: some configs could not be inspected')
    else:
        try:
            path, keys = probe.verdict_store()
            miss = {k: [t for t, cid in seen[k]['songs'].items() if f'{cid}.p' not in keys] for k in snares}
            miss = {k: m for k, m in miss.items() if m}
            n = sum(len(seen[k]['songs']) for k in snares)
            add(not miss, 'kick verdicts for snare songs',
                f'{n} exam and holdout songs of {len(snares)} snare config(s) all in {path.name}' if not miss
                else f'{sum(map(len, miss.values()))} exam or holdout song(s) missing from {path.name} '
                     f'(keys <content id>.p)',
                [f'{c}: {len(m)} song(s): ' + ' '.join(m) for (c, _), m in miss.items()])
        except Exception as e:
            add(False, 'kick verdicts for snare songs', str(e))

    gone = []
    for rid in exams:
        p = probe.runs / rid / 'meta.json'
        try:
            exam = json.loads(p.read_text()).get('exam')
            if not exam:
                gone.append(f'{rid}: {p} has no exam list')
        except FileNotFoundError:
            gone.append(f'{rid}: no {p}')
        except Exception as e:
            gone.append(f'{rid}: {p} unreadable ({e})')
    if plan_err:
        add(False, '--exam-of runs', f'no plan to read ({plan_err})')
    else:
        add(not gone, '--exam-of runs', (f'{len(exams)} run(s) with an exam list: ' + ', '.join(exams)) if exams and not gone
            else 'no --exam-of in the plan' if not gone else f'{len(gone)} of {len(exams)} unusable', gone)

    cold = [f'{c}{" --release" if rel else ""}: {m}' for (c, rel), m in inspected.items()]
    for (c, rel), v in seen.items():
        by = {}
        for song, art in v['misses']:
            by.setdefault(art, []).append(song)
        cold += [f'{c}: {art} missing for {len(s)} song(s): ' + ', '.join(s[:4]) + (' ...' if len(s) > 4 else '')
                 for art, s in sorted(by.items())]
    if plan_err:
        add(False, 'caches warm', f'no plan to read ({plan_err})')
    else:
        songs = sum(len(v['misses']) for v in seen.values())
        add(not cold, 'caches warm', f'{len(seen)} config(s), nothing missing' if not cold
            else f'{songs} artifact(s) missing across {len(seen)} inspected config(s)'
            + (f', {len(inspected)} config(s) not inspectable' if inspected else ''), listed(cold, 40))

    try:
        free_gpu, text, alive = probe.gpu()
        if free_gpu:
            add(True, 'GPU lock', 'free')
        else:
            add(False, 'GPU lock', text + ('; the holder is live, so the plan would wait behind it' if alive
                                           else '; the holder process is dead but something still holds the lock'))
    except Exception as e:
        add(False, 'GPU lock', f'cannot read the lock: {e}')

    add(plan_err is None, 'plan loads', f'{plan_name}: {len(steps)} steps' if not plan_err else plan_err)
    return rows


def render(rows):
    """Lines in check order (the plan check, computed first, is listed last as 9)."""
    out = []
    for n, (ok, title, summary, detail) in enumerate(rows, 1):
        out.append(f'{"PASS" if ok else "FAIL"} {n}. {title}: {summary}')
        out += [f'       {d}' for d in detail]
    return out


def main(argv=None):
    a = sys.argv[1:] if argv is None else argv
    if a[:1] == ['--inspect']:
        from tools.audio_analysis.lab import recipe
        recipe.apply()
        print(json.dumps(inspect_config(a[1], '--release' in a)))
        return 0
    if len(a) != 1 or a[0].startswith('--'):
        sys.exit(__doc__)
    from tools.audio_analysis.lab.overnight import load_plan
    rows = check(a[0], Probe(), load_plan)
    print('\n'.join(render(rows)))
    failed = sum(not r[0] for r in rows)
    print(f'{len(rows) - failed} passed, {failed} failed')
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())
