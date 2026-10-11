"""Model parts (docs/DETECTOR_LAB_DESIGN.md P6): the training plan, the trees and the combine. The net is
kick_goal_nn's; run.py trains it."""
import numpy as np

from tools.audio_analysis.lab.config import COMBINES

# Tree fits run on this many threads (the recipe pins numeric libraries to one for the nets): 3x faster on the kick
# set, the same predictions, and fixed so a fit never depends on the machine.
TREE_THREADS = 4


def plan(cfg, lab, songs):
    """(groups, trainings, untrained songs). A training holds out whole groups (`held`), trains on every other
    trained song and the clips outside them. Training k holds out group k and predicts it. With groups, pair
    training (k, j) holds out both: its predictions choose group k's cutoff on group j and group j's on group k,
    so no cutoff comes from a model that saw the song it scores."""
    from tools.audio_analysis.eval.detector_net import net_groups
    gen = [t for t in lab if songs[t].kind == 'generated']
    untrained = set(gen) if cfg.train == ('project',) or cfg.generated_as_clips() else set()
    clips = gen if cfg.generated_as_clips() else []
    grps = net_groups(list(lab), cfg.folds, untrained)

    def training(without):
        held = [t for k in without for t in grps[k]]
        return dict(without=list(without), held=held, trained=[u for u in lab if u not in held and u not in untrained],
                    clips=[c for c in clips if c not in held])
    out = [training((k,)) for k in range(len(grps))]
    if cfg.folds:
        out += [training((k, j)) for k in range(len(grps)) for j in range(k + 1, len(grps))]
    return grps, out, untrained


def check_disjoint(trainings):
    """No model predicts a song it trained on."""
    for n in trainings:
        seen = set(n['held']) & (set(n['trained']) | set(n['clips']))
        if seen:
            raise ValueError(f'training without groups {n["without"]} trained on {sorted(seen)}')


def save_keys(grps, n):
    """{song: save prefix} for one training's predictions: '' for the outer model of the song's group,
    'pair{k}' for the prediction that chooses group k's cutoff."""
    if len(n['without']) == 1:
        return {t: '' for t in n['held']}
    k, j = n['without']
    return {t: f'pair{j if t in grps[k] else k}' for t in n['held']}


def weights(ys, hards, clip, share=None):
    """Row weights per song, the kick fast loop's: positives total .5, negatives .5 with vouched negatives HARD_W
    times as likely, so every song weighs the same. With a share, the clip songs together weigh share of all."""
    from tools.audio_analysis.eval.kick_goal_melodic import HARD_W
    ws = []
    for y, hard in zip(ys, hards):
        neg = np.where(y == 0, np.where(hard, HARD_W, 1.0), 0.0)
        ws.append(np.where(y == 1, .5 / max(1, y.sum()), .5 * neg / max(1e-9, neg.sum())))
    if share is not None and any(clip) and not all(clip):
        real = sum(w.sum() for w, c in zip(ws, clip) if not c)
        fake = sum(w.sum() for w, c in zip(ws, clip) if c)
        ws = [w * (real * share / (1 - share) / fake) if c else w for w, c in zip(ws, clip)]
    return ws


def fit_trees(p, rows, random_state, share=None):
    """HistGradientBoosting on rows [(x, y, hard, is_clip)], the config's settings."""
    from sklearn.ensemble import HistGradientBoostingClassifier
    from threadpoolctl import threadpool_limits
    w = weights([r[1] for r in rows], [r[2] for r in rows], [r[3] for r in rows], share)
    x, y, w = np.concatenate([r[0] for r in rows]), np.concatenate([r[1] for r in rows]), np.concatenate(w)
    with threadpool_limits(TREE_THREADS, user_api='openmp'):
        return HistGradientBoostingClassifier(
            max_iter=p['max_iter'], learning_rate=p['learning_rate'], max_leaf_nodes=p['max_leaf_nodes'],
            min_samples_leaf=p['min_samples_leaf'], l2_regularization=p['l2'],
            random_state=random_state).fit(x, y, sample_weight=w * len(y))


def combine(how, layers):
    """The combined probability from the model layers {name: probabilities}; mean_logit is kick_release's blend."""
    from scipy.special import expit, logit
    if how != 'mean_logit':
        return layers[how]
    return expit(np.mean([logit(np.clip(layers[m], 1e-6, 1 - 1e-6)) for m in COMBINES[how]], axis=0))


def final_layer(cfg):
    return 'combined' if len(COMBINES[cfg.combine]) > 1 else cfg.combine
