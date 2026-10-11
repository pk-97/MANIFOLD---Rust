#!/usr/bin/env python3
"""Runs an experiment (docs/DETECTOR_LAB_DESIGN.md D6, D7, D10): each seed trains in its own process under the
config's net env, saves held-out probabilities per song, and never scores. The run id hashes the config, the labels
and every tools/audio_analysis module the process imported, so an edit to the code that ran is a new run. Seeds
already saved are skipped. Prints the finish time after each seed.

Usage: run.py CONFIG [--seeds 0,1]     (CONFIG: a lab/experiments name or a path)
       run.py --plan CONFIG            (prints the run id; trains nothing)
       run.py --warm CONFIG            (builds the cached inputs every seed reads; trains nothing)"""
import hashlib
import json
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
TOOLS = ROOT / 'tools' / 'audio_analysis'
RUNS = Path.home() / '.cache/manifold/detector-lab/runs'


def imports():
    """Every tools/audio_analysis module a seed can reach, imported up front so the run id covers all of them,
    whether or not the cache is warm."""
    import tools.audio_analysis.eval.detector_cands  # noqa: F401
    import tools.audio_analysis.eval.detector_channels  # noqa: F401
    import tools.audio_analysis.eval.detector_eval  # noqa: F401
    import tools.audio_analysis.eval.detector_labels  # noqa: F401
    import tools.audio_analysis.eval.detector_net  # noqa: F401
    import tools.audio_analysis.eval.kick_attack_rejection  # noqa: F401
    import tools.audio_analysis.eval.kick_goal_eval  # noqa: F401
    import tools.audio_analysis.eval.kick_goal_nn  # noqa: F401
    import tools.audio_analysis.eval.snare_cands  # noqa: F401
    import tools.audio_analysis.lab.cache  # noqa: F401
    import tools.audio_analysis.lab.registry  # noqa: F401


def check_net(cfg):
    """The net module read the config's env (it reads env once, at import)."""
    from tools.audio_analysis.eval import kick_goal_nn as nn
    got = dict(past_ms=nn.PAST_MS, perc=nn.PERC_ON, bands=nn.BANDS, stereo=nn.STEREO_ON, flat=nn.FLAT_ON,
               ahead=nn.AHEAD_MS)
    want = dict(past_ms=cfg.net['past_ms'], perc=bool(cfg.net['flat']), bands=64, stereo=False, flat=False, ahead=0)
    if got != want:
        sys.exit(f'kick_goal_nn read {got}, the config wants {want}: was it imported before the recipe applied?')


def labels(family):
    """(labels in the legacy order: project files, then generated; sha256 over the files)."""
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.lab.registry import FAMILIES
    lab, h = {}, hashlib.sha256()
    for f in FAMILIES[family]:
        b = (GOAL / f).read_bytes()
        h.update(b)
        lab.update(json.loads(b))
    return lab, h.hexdigest()


def excluded(family):
    """Songs the audit fails (and Peter has not overridden); exits when an audit is missing or older than its labels."""
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.lab.registry import FAMILIES
    out = []
    for f in FAMILIES[family]:
        side = GOAL / f.replace('.json', '.audit.json')
        sha = hashlib.sha256((GOAL / f).read_bytes()).hexdigest()
        if not side.exists() or json.loads(side.read_text())['labels_sha256'] != sha:
            sys.exit(f'{f}: the audit is missing or older than the labels; run lab/audit.py {family}')
        out += [t for t, r in json.loads(side.read_text())['songs'].items() if not r['pass']]
    return sorted(out)


def check_exam(exam, songs, failed=()):
    """The exam holds only project songs that pass the audit."""
    bad = [t for t in exam if songs[t].kind != 'project' or t in failed]
    if bad:
        raise ValueError(f'not exam songs (generated or audit-failing): {bad}')


def plan(cfg, lab, songs):
    """(nets: each its held-out group, trained songs and clips; untrained songs). Mirrors detector_net.held_out_nets,
    which records what it really did; the child compares the two."""
    from tools.audio_analysis.eval.detector_net import net_groups
    gen = [t for t in lab if songs[t].kind == 'generated']
    untrained = set(gen) if cfg.train == ('project',) or cfg.generated_as_clips() else set()
    clips = gen if cfg.generated_as_clips() else []
    nets = [dict(group=g, trained=[u for u in lab if u not in g and u not in untrained],
                 clips=[c for c in clips if c not in g]) for g in net_groups(list(lab), cfg.folds, untrained)]
    return nets, untrained


def check_disjoint(nets):
    """No net predicts a song it trained on."""
    for n in nets:
        seen = set(n['group']) & (set(n['trained']) | set(n['clips']))
        if seen:
            raise ValueError(f'net holding out {n["group"]} trained on {sorted(seen)}')


def setup(cfg):
    """The run's labels, songs and plan under the pinned recipe; returns everything the run id and the child need."""
    from tools.audio_analysis.lab import recipe
    recipe.apply(cfg.net_env())
    imports()
    check_net(cfg)
    from tools.audio_analysis.lab.registry import load
    lab, sha = labels(cfg.family)
    songs = load()
    failed = excluded(cfg.family)
    lab = {t: s for t, s in lab.items() if t not in failed and s['positives']}
    for t, s in lab.items():
        if (s.get('kind') == 'generated') != (songs[t].kind == 'generated'):
            sys.exit(f'{t}: labels and song list disagree on its kind; rebuild with lab/registry.py build')
    exam = [t for t in lab if songs[t].kind == 'project']
    check_exam(exam, songs, failed)
    nets, untrained = plan(cfg, lab, songs)
    check_disjoint(nets)
    mods = sorted((m.__name__, hashlib.sha256(Path(m.__file__).read_bytes()).hexdigest())
                  for m in list(sys.modules.values())
                  if getattr(m, '__file__', None) and Path(m.__file__).resolve().is_relative_to(TOOLS))
    key = json.dumps(dict(config=cfg.key(), labels=sha, modules=mods), sort_keys=True)
    run_id = f'{cfg.family}-{hashlib.sha256(key.encode()).hexdigest()[:12]}'
    meta = dict(run_id=run_id, config=cfg.key(), names=[cfg.name], labels_sha256=sha, songs=list(lab), exam=exam,
                generated=[t for t in lab if t not in exam], excluded=failed,
                recall_only=[t for t in exam if lab[t]['positives_only']], nets=nets, modules=mods)
    return lab, songs, meta


def build(cfg, lab, songs, dev, log=print):
    """{song: kick_goal_nn.Song} from the cache: the retired snare runner's inputs, byte for byte (test_input_parity)."""
    from tools.audio_analysis.eval.detector_eval import HOP, rows
    from tools.audio_analysis.eval.kick_goal_nn import Song
    from tools.audio_analysis.lab import cache
    ahead = cfg.net['ahead_ms'] / 1000
    data = {}
    for t, s in lab.items():
        song = songs[t]
        cand, avail = cache.candidates(song, cfg.candidates, cfg.family)
        onset, emit = (cand + 1) * HOP / song.sample_rate, (avail + 1) * HOP / song.sample_rate
        mask, y, hard = rows(s, onset)
        extra = (cache.flatness(song),) if cfg.net['flat'] else ()
        data[t] = Song(cache.spectrum(song), onset, emit + ahead, mask, y, t, hard, extra).to(dev)
        if song.kind == 'generated':
            data[t].blocked = {t}
        log(f'{t}: {len(cand)} candidates, {int(mask.sum())} training rows')
    return data


def child(cfg, seed, run_id):
    lab, songs, meta = setup(cfg)
    if meta['run_id'] != run_id:
        sys.exit(f'run id {meta["run_id"]} is not the planned {run_id}: code or labels changed mid-run')
    sys.path.insert(0, str(ROOT / 'scripts'))
    import gpu_queue
    from tools.audio_analysis.eval.detector_net import held_out_nets
    from tools.audio_analysis.eval.kick_goal_nn import device
    out = RUNS / run_id
    out.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    dev = device()
    data = build(cfg, lab, songs, dev, log=lambda m: None)
    import numpy as np
    nets, untrained = plan(cfg, lab, songs)
    clips = [data[t] for t in meta['generated']] if cfg.generated_as_clips() else []
    kw = dict(epochs=cfg.net['epochs'])
    if cfg.generated_as_clips():
        kw['synth_share'] = cfg.sampling['generated']
    built = time.time()
    record = []
    with gpu_queue.hold('detector-lab'):
        held = held_out_nets(lab, data, seed, clips, log=lambda m: print(m, flush=True), folds=cfg.folds,
                             untrained=untrained, train_kw=kw, record=record)
    if [(r['group'], r['trained'], r['clips']) for r in record] != [(n['group'], n['trained'], n['clips']) for n in nets]:
        sys.exit('the nets trained differ from the plan; nothing saved')
    np.savez(out / f's{seed}.npz', **held)
    (out / f's{seed}.json').write_text(json.dumps(dict(seed=seed, device=str(dev), build_s=round(built - t0, 1),
                                                       train_s=round(time.time() - built, 1), nets=record), indent=1))
    meta_path = out / 'meta.json'
    if meta_path.exists():
        old = json.loads(meta_path.read_text())
        meta['names'] = sorted(set(old['names']) | set(meta['names']))
    meta_path.write_text(json.dumps(meta, indent=1))


def warm(cfg):
    """Builds every cached input a seed reads (candidates, spectra, noisiness), so seed timings are warm."""
    from tools.audio_analysis.lab import cache
    lab, songs, _ = setup(cfg)
    for t in lab:
        t0 = time.time()
        cache.candidates(songs[t], cfg.candidates, cfg.family)
        cache.spectrum(songs[t])
        if cfg.net['flat']:
            cache.flatness(songs[t])
        print(f'{t}: {time.time() - t0:.1f} s', flush=True)


def run_id_of(config):
    """The run id a seed of this config would save under (a separate process, so the parent's imports don't count)."""
    r = subprocess.run([str(Path(__file__)), '--plan', str(config)], capture_output=True, text=True)
    if r.returncode:
        sys.exit(r.stderr or r.stdout)
    return r.stdout.strip().splitlines()[-1]


def run(config, seeds=None):
    """Trains the config's missing seeds, one process each; returns the run id."""
    from tools.audio_analysis.lab.config import load
    cfg = load(config)
    seeds = cfg.seeds if seeds is None else seeds
    run_id = run_id_of(config)
    todo = [s for s in seeds if not (RUNS / run_id / f's{s}.npz').exists()]
    print(f'{cfg.name}: run {run_id}, seeds {list(seeds)}, {len(todo)} to train', flush=True)
    for k, seed in enumerate(todo):
        t0 = time.time()
        r = subprocess.run([str(Path(__file__)), '--child', str(config), str(seed), run_id])
        if r.returncode:
            sys.exit(f'{cfg.name} seed {seed} failed')
        took = time.time() - t0
        left = len(todo) - k - 1
        print(f'{cfg.name} seed {seed}: {took / 60:.1f} min'
              + (f'; {left} left, finish about {time.strftime("%H:%M", time.localtime(time.time() + left * took))}' if left else ''),
              flush=True)
    return run_id


def main():
    a = sys.argv[1:]
    if a[:1] == ['--plan']:
        from tools.audio_analysis.lab.config import load
        print(setup(load(a[1]))[2]['run_id'])
    elif a[:1] == ['--warm']:
        from tools.audio_analysis.lab.config import load
        warm(load(a[1]))
    elif a[:1] == ['--child']:
        from tools.audio_analysis.lab.config import load
        child(load(a[1]), int(a[2]), a[3])
    elif a:
        seeds = [int(s) for s in a[a.index('--seeds') + 1].split(',')] if '--seeds' in a else None
        run(a[0], seeds)
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main()
