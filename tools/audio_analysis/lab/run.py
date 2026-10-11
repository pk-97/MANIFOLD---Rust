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
LAYERS = RUNS.parent / 'layers'   # one trained model's predictions, shared by every config that trains it
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
    import tools.audio_analysis.eval.kick_goal_selfsim  # noqa: F401
    import tools.audio_analysis.eval.snare_cands  # noqa: F401
    import tools.audio_analysis.lab.cache  # noqa: F401
    import tools.audio_analysis.lab.components  # noqa: F401
    import tools.audio_analysis.lab.models  # noqa: F401
    import tools.audio_analysis.lab.registry  # noqa: F401
    import tools.audio_analysis.lab.stage  # noqa: F401
    import tools.audio_analysis.lab.templates  # noqa: F401


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
    if cfg.stage:
        from tools.audio_analysis.lab.stage import measures_of
        out['stage'] = {m: code_id(components.MEASURES[m]) for m in measures_of(cfg.stage)}
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


def module_hashes():
    return sorted((m.__name__, hashlib.sha256(Path(m.__file__).read_bytes()).hexdigest())
                  for m in list(sys.modules.values())
                  if getattr(m, '__file__', None) and Path(m.__file__).resolve().is_relative_to(TOOLS))


def setup(cfg):
    """The run's labels, songs and plan under the pinned recipe; returns everything the run id and the child need."""
    from tools.audio_analysis.lab import models, recipe
    recipe.apply(cfg.net_env())
    imports()
    check_net(cfg)
    from tools.audio_analysis.lab.registry import load
    from tools.audio_analysis.lab.registry import check_sources
    lab, sha = labels(cfg.family)
    songs = load()
    failed = excluded(cfg.family)
    lab = {t: s for t, s in lab.items() if t not in failed}
    check_sources([songs[t] for t in lab])
    content = {t: songs[t].content_id for t in lab}
    label_ids = {t: hashlib.sha256(json.dumps(s, sort_keys=True).encode()).hexdigest()[:16] for t, s in lab.items()}
    for t, s in lab.items():
        if (s.get('kind') == 'generated') != (songs[t].kind == 'generated'):
            sys.exit(f'{t}: labels and song list disagree on its kind; rebuild with lab/registry.py build')
    exam = [t for t in lab if songs[t].kind == 'project']
    check_exam(exam, songs, failed)
    grps, trainings, _ = models.plan(cfg, lab, songs)
    models.check_disjoint(trainings)
    used = parts(cfg)
    mods = module_hashes()
    key = json.dumps(dict(config=cfg.key(), labels=sha, modules=mods, parts=used, content=content), sort_keys=True)
    run_id = f'{cfg.family}-{hashlib.sha256(key.encode()).hexdigest()[:12]}'
    meta = dict(schema=META_SCHEMA, run_id=run_id, config=cfg.key(), names=[cfg.name], labels_sha256=sha,
                songs=list(lab), exam=exam, generated=[t for t in lab if t not in exam], excluded=failed,
                recall_only=[t for t in exam if lab[t]['positives_only']], groups=grps, trainings=trainings,
                layers=[*sorted(cfg.models), *(['combined'] if models.final_layer(cfg) == 'combined' else []),
                        *(['staged'] if cfg.stage else [])],
                final='staged' if cfg.stage else models.final_layer(cfg), modules=mods, parts=used, content=content,
                label_ids=label_ids)
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


def model_code(model, with_templates=False):
    """The code that trains a model and builds its inputs, closure included (cache.code_id); trees with sound
    templates add the templates' training and patch code."""
    from tools.audio_analysis.lab import templates
    from tools.audio_analysis.lab.cache import code_id
    code = code_id(train_model) + code_id(build if model == 'net' else tree_rows)
    return code + code_id(train_templated) + code_id(templates.song_inputs) if with_templates else code


def templated(cfg):
    return 'trees' in cfg.models and cfg.models['trees']['templates'] is not None


def model_key(cfg, model, meta):
    """What decides one model's predictions in a run: its own settings, the finder, sampling and plan, the labels of
    the songs it trains on (never the held songs'), every song's audio, the training code, and the parts it reads.
    Configs that share it share the trained layer (LAYERS); relabelling a song nobody trains on retrains nothing."""
    sub = {k: v for k, v in cfg.key().items() if k not in ('models', 'combine', 'stage')}
    sub['models'] = {model: cfg.models[model]}
    reads = dict(net='frames', trees='measures')[model]
    used = {k: v for k, v in meta['parts'].items() if k in ('finder', reads)}
    taught = sorted({u for n in meta['trainings'] for u in n['trained'] + n['clips']})
    code = model_code(model, model == 'trees' and templated(cfg))
    key = json.dumps(dict(model=sub, labels={u: meta['label_ids'][u] for u in taught}, code=code, parts=used,
                          content=meta['content'], trainings=meta['trainings']), sort_keys=True)
    return f'{model}-{hashlib.sha256(key.encode()).hexdigest()[:12]}'


def train_model(cfg, model, trainings, seed, data, trees, log):
    """[{held song: probabilities} per training], [seconds per training] for one model. Training i uses random state
    1000 * i + seed, so the outer nets match the retired runner's."""
    from tools.audio_analysis.lab import models
    share = cfg.sampling['generated'] if cfg.generated_as_clips() else None
    out, secs = [], []
    for i, n in enumerate(trainings):
        t0 = time.time()
        if model == 'net':
            from tools.audio_analysis.eval.kick_goal_nn import predict, train
            kw = dict(epochs=cfg.net['epochs'], swa_epochs=cfg.net['swa_epochs'], batch=cfg.net['batch'], lr=cfg.net['lr'],
                      precision=cfg.net['precision'], **({'synth_share': share} if share is not None else {}))
            net = train([data[u] for u in n['trained']], 1000 * i + seed, [data[c] for c in n['clips']], **kw)
            out.append({t: predict(net, data[t]) for t in n['held']})
        else:
            fit = models.fit_trees(cfg.models['trees'], [(*trees[u][1], u in n['clips']) for u in n['trained'] + n['clips']],
                                   1000 * i + seed, share)
            out.append({t: fit.predict_proba(trees[t][0])[:, 1] for t in n['held']})
        secs.append(round(time.time() - t0, 1))
        log(f'{model} without groups {n["without"]}: {secs[-1]} s')
    return out, secs


def train_templated(cfg, trainings, seed, trees, tmpl, log):
    """train_model's trees with sound templates (lab/templates.py) appended to every song's measures: each training
    fits its own templates on its trained songs and clips (never its held songs), random state 1000 i + seed for the
    templates and the trees alike."""
    import numpy as np

    from tools.audio_analysis.lab import templates
    share = cfg.sampling['generated'] if cfg.generated_as_clips() else None
    templates.span(tmpl)
    out, secs = [], []
    for i, n in enumerate(trainings):
        t0 = time.time()
        rs = 1000 * i + seed
        fitted = templates.fit({u: tmpl[u] for u in n['trained'] + n['clips']}, set(n['clips']),
                               cfg.models['trees']['templates']['k'], rs, share)
        rows = {}
        for u in n['trained'] + n['clips'] + n['held']:
            x = np.hstack([trees[u][0], templates.features(tmpl[u][0], fitted)])
            mask, y, hard = tmpl[u][1]
            rows[u] = (x, (x[mask], y[mask], hard[mask]))
        took = time.time() - t0
        got, s = train_model(cfg, 'trees', [n], rs, None, rows, log)
        out += got
        secs.append(round(s[0] + took, 1))
    return out, secs


def layer(cfg, model, meta, seed, data, trees, log, tmpl=None):
    """One model's predictions per training and their training seconds: from LAYERS when another config already
    trained the same model, else trained and stored there. Cached seconds are 0."""
    import numpy as np
    d = LAYERS / model_key(cfg, model, meta)
    path = d / f's{seed}.npz'
    if path.exists():
        with np.load(path) as z:
            out = [{t: z[f'{i}__{t}'] for t in n['held']} for i, n in enumerate(meta['trainings'])]
        log(f'{model}: reused {d.name}')
        return out, [0.0] * len(out)
    if model == 'trees' and templated(cfg):
        out, secs = train_templated(cfg, meta['trainings'], seed, trees, tmpl, log)
    else:
        out, secs = train_model(cfg, model, meta['trainings'], seed, data, trees, log)
    d.mkdir(parents=True, exist_ok=True)
    tmp = d / f's{seed}.tmp.npz'
    np.savez(tmp, **{f'{i}__{t}': p for i, per in enumerate(out) for t, p in per.items()})
    tmp.rename(path)
    return out, secs


def train_layers(cfg, meta, seed, data, trees, log=print, stage_parts=None, tmpl=None):
    """({save key: probabilities}, record): each model's layer, then the combined one, saved per song as layer__song
    (outer models) and pair{k}__layer__song (the predictions that choose group k's cutoff); with a stage, its
    staged layer on top (lab/stage.py, from stage_parts). tmpl: the trees' sound-template inputs, when templated."""
    from tools.audio_analysis.lab import models
    trainings, grps = meta['trainings'], meta['groups']
    lay, secs = {}, {}
    for m in sorted(cfg.models):
        lay[m], secs[m] = layer(cfg, m, meta, seed, data, trees, log, tmpl)
    if models.final_layer(cfg) == 'combined':
        lay['combined'] = [{t: models.combine(cfg.combine, {m: lay[m][i][t] for m in cfg.models}) for t in n['held']}
                           for i, n in enumerate(trainings)]
    record = [dict(n, seconds={m: secs[m][i] for m in secs}) for i, n in enumerate(trainings)]
    held = {}
    for i, n in enumerate(trainings):
        for t, prefix in models.save_keys(grps, n).items():
            for name in lay:
                held[f'{prefix}__{name}__{t}' if prefix else f'{name}__{t}'] = lay[name][i][t]
    if cfg.stage:
        from tools.audio_analysis.lab import stage
        t0 = time.time()
        held.update(stage.train(cfg, grps, trainings, lay[models.final_layer(cfg)], stage_parts, seed,
                                generated=meta['generated'], log=log))
        record = [dict(r, seconds=dict(r['seconds'], stage=round(time.time() - t0, 1) if i == 0 else 0.0))
                  for i, r in enumerate(record)]
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
    todo = [m for m in cfg.models if not (LAYERS / model_key(cfg, m, meta) / f's{seed}.npz').exists()]
    data = build(cfg, lab, songs, dev, log=lambda m: None) if 'net' in todo else None
    trees = tree_rows(cfg, lab, songs) if 'trees' in todo else None
    if 'trees' in todo and templated(cfg):
        from tools.audio_analysis.lab import templates
        tmpl = templates.song_inputs(cfg, lab, songs)
    else:
        tmpl = None
    if cfg.stage:
        from tools.audio_analysis.lab.stage import song_parts
        stage_parts = {t: song_parts(cfg, songs[t], lab[t]) for t in lab}
    else:
        stage_parts = None
    built = time.time()
    with gpu_queue.hold('detector-lab') if 'net' in todo else contextlib.nullcontext():
        held, record = train_layers(cfg, meta, seed, data, trees, log=lambda m: print(m, flush=True),
                                    stage_parts=stage_parts, tmpl=tmpl)
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
        if templated(cfg):
            from tools.audio_analysis.lab import templates
            templates.patches(songs[t], cfg.candidates, cfg.family)
        if cfg.stage:
            from tools.audio_analysis.lab.stage import measures_of
            for m in measures_of(cfg.stage):
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
