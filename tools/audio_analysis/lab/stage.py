"""The song-history stage (docs/DETECTOR_LAB_DESIGN.md P6): a plug-in on top of any detector. Each candidate is
judged against the song's own recent confident hits (kick_goal_selfsim: its score and sound against theirs, and how
much evidence the recent past holds), then trees re-score it. Causal: only candidates decided earlier count. The
config names the shape measures, an optional level column and the memory windows, so any family can use it.

No stage ever judges a song with inputs from a model that saw it. Per seed and group k:
- the stage for group k learns on every other group j, inputs built from the pair (k, j) predictions, and judges
  group k on the outer predictions (without k): the shipped kick's recipe;
- group k's cutoff: for each other group j, a stage learnt on the groups other than k and j (their pair (k, i)
  inputs) judges group j on its pair (k, j) inputs, saved as pair{k}__staged.

History only speaks once it has heard something (gate > 0): with an empty memory the final score is the detector's
own, and history's change fades in until the memory holds `gate` of evidence (the sum of p ** 8 over the first
window; gate 1 is one or two confident hits). Trained on music, a stage learns that a hit rarely follows a long
silence, which is exactly where a drop's first hit lands; the gate takes that lesson away. `learn` 'project' learns on real songs
only: generated sections are short, so much of their memory is empty."""
import numpy as np

STAGE_KEYS = {'windows_s', 'shape', 'level', 'learn', 'gate', 'max_iter', 'learning_rate', 'max_leaf_nodes',
              'min_samples_leaf', 'l2'}


def check(st, folds, measures, where=''):
    if set(st) != STAGE_KEYS:
        raise ValueError(f'{where}: stage keys are {sorted(STAGE_KEYS)}')
    if folds < 2:
        raise ValueError(f'{where}: a stage needs song groups (folds >= 2) to learn on predictions of unseen songs')
    if not st['windows_s'] or any(not w > 0 for w in st['windows_s']):
        raise ValueError(f'{where}: stage windows_s is a list of positive seconds')
    if not st['shape'] or set(st['shape']) - set(measures):
        raise ValueError(f'{where}: stage shape measures from {sorted(measures)}')
    lv = st['level']
    if lv is not None and not (len(lv) == 2 and lv[0] in measures and isinstance(lv[1], int) and lv[1] >= 0):
        raise ValueError(f'{where}: stage level is null or [measure, column]')
    if st['learn'] not in ('all', 'project') or not (isinstance(st['gate'], (int, float)) and st['gate'] >= 0):
        raise ValueError(f'{where}: stage learn is "all" or "project", gate >= 0 (0: off)')


def gated(st, x, q, cuts=(.5, .5)):
    """Stage probabilities q on inputs x with the gate. Scores are measured against their own cutoffs (cuts: the
    detector's, the stage's), since history moves confident hits far up and its cutoff with them: the detector's
    distance from its cutoff plus history's change in that distance, scaled by the first window's evidence over
    st['gate'], put back on the detector's scale. With an empty memory a hit fires exactly when the detector's would."""
    from scipy.special import expit, logit
    if not st['gate']:
        return q
    evidence = np.expm1(x[:, 4 if st['level'] else 3])
    g = np.minimum(1.0, evidence / st['gate'])
    det = x[:, 0] - logit(cuts[0])
    stg = logit(np.clip(q, 1e-6, 1 - 1e-6)) - logit(cuts[1])
    return expit(logit(cuts[0]) + det + g * (stg - det))


def measures_of(st):
    return sorted(set(st['shape']) | ({st['level'][0]} if st['level'] else set()))


def song_parts(cfg, song, lab, build=True):
    """(shape (n, k), level (n,) or None, emit_s, training rows (mask, y, hard), scorer meta (models.song_meta)) for
    one song's candidates."""
    from tools.audio_analysis.eval.detector_eval import rows
    from tools.audio_analysis.lab import components
    from tools.audio_analysis.lab.models import song_meta
    st = cfg.stage
    c = components.candidates(song, cfg.candidates, cfg.family, build)
    got = {m: components.measure(song, cfg.candidates, cfg.family, m, build) for m in measures_of(st)}
    shape = np.hstack([got[m] for m in st['shape']])
    level = got[st['level'][0]][:, st['level'][1]] if st['level'] else None
    return shape, level, c.emit_s, rows(lab, c.onset_s), song_meta(lab, c)


def inputs(st, part, p):
    """One song's stage inputs from its detector probabilities p: logit p once, then per window the likeness to
    the recent hits' sound, p and level against theirs (level only when the config names one) and the evidence."""
    from tools.audio_analysis.eval.kick_goal_selfsim import self_features
    shape, level, emit = part[:3]
    keep = [1, 2, 3, 4] if st['level'] else [1, 2, 4]
    xs = [self_features(p, shape, level if st['level'] else np.zeros(len(p)), emit, w) for w in st['windows_s']]
    return np.hstack([xs[0][:, :1]] + [x[:, keep] for x in xs])


def train(cfg, grps, trainings, final, parts, seed, generated=(), log=print):
    """{save key: staged probabilities}. final[i] = {held song: the detector's probabilities} for trainings[i] (plan
    order); parts[song] = song_parts. Learns on the songs the detector trains on (generated ones as clips, weighted
    by the config's share), less the generated songs when learn is 'project'."""
    import time

    from tools.audio_analysis.lab import models
    st = cfg.stage
    at = {tuple(sorted(n['without'])): i for i, n in enumerate(trainings)}
    learn = {u for n in trainings for u in n['trained'] + n['clips']}
    if st['learn'] == 'project':
        learn -= set(generated)
    clips = {u for n in trainings for u in n['clips']}
    share = cfg.sampling['generated'] if cfg.generated_as_clips() else None
    x = {}

    def stage_x(k, j, u):
        """Inputs of song u (group j) from the models without groups k and j (the outer one when j == k)."""
        if (k, j, u) not in x:
            x[k, j, u] = inputs(st, parts[u], final[at[tuple(sorted({k, j}))]][u])
        return x[k, j, u]

    def fit(k, groups, random_state):
        rows = []
        for i in groups:
            for u in grps[i]:
                if u in learn:
                    mask, y, hard = parts[u][3]
                    rows.append((stage_x(k, i, u)[mask], y[mask], hard[mask], u in clips))
        return models.fit_trees(st, rows, random_state, share)

    out = {}
    t0 = time.time()
    for k, g in enumerate(grps):
        others = [j for j in range(len(grps)) if j != k]
        outer = fit(k, others, 1000 * k + seed)
        # Group k's pool: the other groups judged by stages and detectors that saw neither them nor k. The gate's
        # cutoffs come from it too, so nothing of group k sets them.
        pool = {}
        for j in others:
            m = fit(k, [i for i in others if i != j], 1000 * k + 100 * (j + 1) + seed)
            pool.update({u: (j, m.predict_proba(stage_x(k, j, u))[:, 1]) for u in grps[j]})
        cuts = None
        if st['gate']:
            metas = {u: parts[u][4] for u in pool}
            cuts = (models.cutoff(metas, {u: final[at[tuple(sorted((k, j)))]][u] for u, (j, _) in pool.items()}),
                    models.cutoff(metas, {u: q for u, (_, q) in pool.items()}))
        for u, (j, q) in pool.items():
            out[f'pair{k}__staged__{u}'] = gated(st, stage_x(k, j, u), q, cuts)
        for t in g:
            out[f'staged__{t}'] = gated(st, stage_x(k, k, t), outer.predict_proba(stage_x(k, k, t))[:, 1], cuts)
    log(f'stage: {len(grps) ** 2} fits in {time.time() - t0:.1f} s')
    return out
