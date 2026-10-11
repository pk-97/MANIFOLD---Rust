#!/usr/bin/env python3
"""Runs an experiment (docs/DETECTOR_LAB_DESIGN.md D6, D7, D10, P6): each seed trains in its own process under the
config's net env, saves held-out probabilities per song and layer (each model, then the combined), and never scores.
With song groups it also trains the pair models that choose each group's cutoff (lab/models.py plan). The run id hashes the config, the labels
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
META_SCHEMA = 2


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
    import tools.audio_analysis.lab.components  # noqa: F401
    import tools.audio_analysis.lab.models  # noqa: F401
    import tools.audio_analysis.lab.registry  # noqa: F401


def parts(cfg):
    """The code of every part the config names: the finder, each measure's closure. Computing it imports the parts'
    modules, so a run's module list is the same whether or not the cache is warm."""
    from tools.audio_analysis.lab import components
    from tools.audio_analysis.lab.cache import code_id
    finder = components.FINDERS[cfg.candidates.split(':')[0]](cfg.candidates, None, cfg.family)[0]
    out = dict(finder=finder)
    if cfg.net:
        out['frames'] = {m: code_id(components.FRAME_MEASURES[m]) for m in cfg.net['channels']}
    if 'trees' in cfg.models:
        out['measures'] = {m: code_id(components.MEASURES[m]) for m in cfg.models['trees']['measures']}
    return out


def check_net(cfg):
    """The net module read the config's env (it reads env once, at import)."""
    from tools.audio_analysis.eval import kick_goal_nn as nn
    got = dict(past_ms=nn.PAST_MS, perc=nn.PERC_ON, bands=nn.BANDS, stereo=nn.STEREO_ON, flat=nn.FLAT_ON,
               ahead=nn.AHEAD_MS)
    want = dict(past_ms=cfg.net['past_ms'] if cfg.net else 150, perc=False, bands=64, stereo=False, flat=False, ahead=0)
    if got != want:
        sys.exit(f'kick_goal_nn read {got}, the config wants {want}: was it imported before the recipe applied?')


def labels(family):
    """(labels in the legacy order: project files, then generated; sha256 over the files). A song in two files is
    an error."""
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.lab.registry import FAMILIES
    lab, h = {}, hashlib.sha256()
    for f in FAMILIES[family]:
        b = (GOAL / f).read_bytes()
        h.update(b)
        d = json.loads(b)
        twice = sorted(set(d) & set(lab))
        if twice:
            raise ValueError(f'{family}: songs in two labels files: {twice}')
        lab.update(d)
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


def setup(cfg):
    """The run's labels, songs and plan under the pinned recipe; returns everything the run id and the child need."""
    from tools.audio_analysis.lab import models, recipe
    recipe.apply(cfg.net_env())
    imports()
    check_net(cfg)
    from tools.audio_analysis.lab.registry import load
    lab, sha = labels(cfg.family)
    songs = load()
    failed = excluded(cfg.family)
    lab = {t: s for t, s in lab.items() if t not in failed}
    for t, s in lab.items():
        if (s.get('kind') == 'generated') != (songs[t].kind == 'generated'):
            sys.exit(f'{t}: labels and song list disagree on its kind; rebuild with lab/registry.py build')
    exam = [t for t in lab if songs[t].kind == 'project']
    check_exam(exam, songs, failed)
    grps, trainings, _ = models.plan(cfg, lab, songs)
    models.check_disjoint(trainings)
    used = parts(cfg)
    mods = sorted((m.__name__, hashlib.sha256(Path(m.__file__).read_bytes()).hexdigest())
                  for m in list(sys.modules.values())
                  if getattr(m, '__file__', None) and Path(m.__file__).resolve().is_relative_to(TOOLS))
    key = json.dumps(dict(config=cfg.key(), labels=sha, modules=mods, parts=used), sort_keys=True)
    run_id = f'{cfg.family}-{hashlib.sha256(key.encode()).hexdigest()[:12]}'
    meta = dict(schema=META_SCHEMA, run_id=run_id, config=cfg.key(), names=[cfg.name], labels_sha256=sha,
                songs=list(lab), exam=exam, generated=[t for t in lab if t not in exam], excluded=failed,
                recall_only=[t for t in exam if lab[t]['positives_only']], groups=grps, trainings=trainings,
                layers=[*sorted(cfg.models), *(['combined'] if models.final_layer(cfg) == 'combined' else [])],
                final=models.final_layer(cfg), modules=mods, parts=used)
    return lab, songs, meta


def build(cfg, lab, songs, dev, log=print):
    """{song: kick_goal_nn.Song} for the net from the cache: the retired snare runner's inputs, byte for byte
    (test_input_parity). Frame measures are extra channels."""
    from tools.audio_analysis.eval.detector_eval import rows
    from tools.audio_analysis.eval.kick_goal_nn import Song
    from tools.audio_analysis.lab import cache, components
    ahead = cfg.ahead_s
    data = {}
    for t, s in lab.items():
        song = songs[t]
        c = components.candidates(song, cfg.candidates, cfg.family)
        cand, onset, emit = c.cand, c.onset_s, c.emit_s
        mask, y, hard = rows(s, onset)
        extra = tuple(components.frame(song, m) for m in cfg.net['channels'])
        data[t] = Song(cache.spectrum(song), onset, emit + ahead, mask, y, t, hard, extra).to(dev)
        if song.kind == 'generated':
            data[t].blocked = {t}
        log(f'{t}: {len(cand)} candidates, {int(mask.sum())} training rows')
    return data


def tree_rows(cfg, lab, songs):
    """{song: (all measures (n, k), training rows (x, y, hard))} for the trees, from the cache."""
    import numpy as np
    from tools.audio_analysis.eval.detector_eval import rows
    from tools.audio_analysis.lab import components
    out = {}
    for t, s in lab.items():
        song = songs[t]
        c = components.candidates(song, cfg.candidates, cfg.family)
        x = np.hstack([components.measure(song, cfg.candidates, cfg.family, m) for m in cfg.models['trees']['measures']])
        if len(x) != len(c.cand):
            raise ValueError(f'{t}: measures have {len(x)} rows for {len(c.cand)} candidates')
        mask, y, hard = rows(s, c.onset_s)
        out[t] = (x, (x[mask], y[mask], hard[mask]))
    return out


def train_layers(cfg, lab, songs, seed, data, trees, log=print):
    """({save key: probabilities}, record). Training i of the plan uses random state 1000 * i + seed, so the outer
    nets match the retired runner's."""
    from tools.audio_analysis.eval.kick_goal_nn import predict, train
    from tools.audio_analysis.lab import models
    grps, trainings, _ = models.plan(cfg, lab, songs)
    share = cfg.sampling['generated'] if cfg.generated_as_clips() else None
    kw = dict(epochs=cfg.net['epochs']) if cfg.net else {}
    if share is not None:
        kw['synth_share'] = share
    layers, record = [{} for _ in trainings], []
    for i, n in enumerate(trainings):
        rec = dict(n, seconds={})
        if cfg.net:
            t0 = time.time()
            net = train([data[u] for u in n['trained']], 1000 * i + seed, [data[c] for c in n['clips']], **kw)
            layers[i]['net'] = {t: predict(net, data[t]) for t in n['held']}
            rec['seconds']['net'] = round(time.time() - t0, 1)
        if trees is not None:
            t0 = time.time()
            fit = models.fit_trees(cfg.models['trees'], [(*trees[u][1], u in n['clips']) for u in n['trained'] + n['clips']],
                                   1000 * i + seed, share)
            layers[i]['trees'] = {t: fit.predict_proba(trees[t][0])[:, 1] for t in n['held']}
            rec['seconds']['trees'] = round(time.time() - t0, 1)
        if models.final_layer(cfg) == 'combined':
            layers[i]['combined'] = {t: models.combine(cfg.combine, {m: layers[i][m][t] for m in cfg.models})
                                     for t in n['held']}
        record.append(rec)
        log(f'without groups {n["without"]}: {rec["seconds"]}')
    held = {}
    for n, lay in zip(trainings, layers):
        for t, prefix in models.save_keys(grps, n).items():
            for name, probs in lay.items():
                held[f'{prefix}__{name}__{t}' if prefix else f'{name}__{t}'] = probs[t]
    return held, record


def child(cfg, seed, run_id):
    lab, songs, meta = setup(cfg)
    if meta['run_id'] != run_id:
        sys.exit(f'run id {meta["run_id"]} is not the planned {run_id}: code or labels changed mid-run')
    sys.path.insert(0, str(ROOT / 'scripts'))
    import contextlib

    import gpu_queue
    import numpy as np
    from tools.audio_analysis.eval.kick_goal_nn import device
    out = RUNS / run_id
    out.mkdir(parents=True, exist_ok=True)
    t0 = time.time()
    dev = device()
    data = build(cfg, lab, songs, dev, log=lambda m: None) if cfg.net else None
    trees = tree_rows(cfg, lab, songs) if 'trees' in cfg.models else None
    built = time.time()
    with gpu_queue.hold('detector-lab') if cfg.net else contextlib.nullcontext():
        held, record = train_layers(cfg, lab, songs, seed, data, trees, log=lambda m: print(m, flush=True))
    keys = ('without', 'held', 'trained', 'clips')
    if [{k: r[k] for k in keys} for r in record] != [{k: n[k] for k in keys} for n in meta['trainings']]:
        sys.exit('the models trained differ from the plan; nothing saved')
    np.savez(out / f's{seed}.npz', **held)
    (out / f's{seed}.json').write_text(json.dumps(dict(seed=seed, device=str(dev), build_s=round(built - t0, 1),
                                                       train_s=round(time.time() - built, 1), trainings=record), indent=1))
    meta_path = out / 'meta.json'
    if meta_path.exists():
        old = json.loads(meta_path.read_text())
        meta['names'] = sorted(set(old['names']) | set(meta['names']))
    meta_path.write_text(json.dumps(meta, indent=1))


def warm(cfg):
    """Builds every cached input a seed reads (candidates, spectra, frame and event measures), so seed timings are
    warm."""
    from tools.audio_analysis.lab import cache, components
    lab, songs, _ = setup(cfg)
    for t in lab:
        t0 = time.time()
        components.candidates(songs[t], cfg.candidates, cfg.family)
        if cfg.net:
            cache.spectrum(songs[t])
            for m in cfg.net['channels']:
                components.frame(songs[t], m)
        for m in cfg.models.get('trees', {}).get('measures', ()):
            components.measure(songs[t], cfg.candidates, cfg.family, m)
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
