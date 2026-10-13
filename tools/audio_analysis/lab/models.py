"""Model parts (docs/DETECTOR_LAB_DESIGN.md P6): the training plan, the trees and the combine. The net is
kick_goal_nn's; run.py trains it."""
import numpy as np

from tools.audio_analysis.lab.config import COMBINES

# Tree fits run on this many threads (the recipe pins numeric libraries to one for the nets): 3x faster on the kick
# set, the same predictions, and fixed so a fit never depends on the machine.
TREE_THREADS = 4


def plan(cfg, lab, songs, final=False):
    """(groups, trainings, untrained songs). A training holds out whole groups (`held`), trains on every other
    trained song and the clips outside them. Training k holds out group k and predicts it. With groups, pair
    training (k, j) holds out both: its predictions choose group k's cutoff on group j and group j's on group k,
    so no cutoff comes from a model that saw the song it scores. final: the one training that holds nothing out,
    the release's final models (lab/release.py)."""
    from tools.audio_analysis.eval.detector_net import net_groups
    gen = [t for t in lab if songs[t].kind == 'generated']
    untrained = set(gen) if cfg.train == ('project',) or cfg.generated_as_clips() else set()
    clips = gen if cfg.generated_as_clips() else []
    # Copies (registry.gain_copy, registry.variant_copy) are not songs of any group: they train with their parent's
    # group, so no model trains on a copy of a song it predicts. Probes are held out with their parent.
    grps = net_groups([t for t in lab if songs[t].parent is None], cfg.folds, untrained)

    def training(without):
        held = [t for k in without for t in grps[k]]
        trained = [u for u in lab if (songs[u].parent or u) not in held and u not in untrained and not songs[u].probe]
        # Probes are predicted by the outer models only: a pair model's predictions choose cutoffs, and probes never do.
        probes = [u for u in lab if songs[u].probe and songs[u].parent in held] if len(without) == 1 else []
        return dict(without=list(without), held=held + probes, trained=trained, clips=[c for c in clips if c not in held])
    if final:
        return grps, [training(())], untrained
    out = [training((k,)) for k in range(len(grps))]
    if cfg.folds:
        out += [training((k, j)) for k in range(len(grps)) for j in range(k + 1, len(grps))]
    return grps, out, untrained


def check_disjoint(trainings, parents=None):
    """No model predicts a song it trained on, nor one whose gain copy it trained on (parents: copy -> its song)."""
    parents = parents or {}
    for n in trainings:
        seen = set(n['held']) & {parents.get(u) or u for u in n['trained'] + n['clips']}
        if seen:
            raise ValueError(f'training without groups {n["without"]} trained on {sorted(seen)}')


def save_keys(grps, n):
    """{song: save prefix} for one training's predictions: '' for the outer model of the song's group,
    'pair{k}' for the prediction that chooses group k's cutoff."""
    if len(n['without']) == 1:
        return {t: '' for t in n['held']}
    k, j = n['without']
    return {t: f'pair{j if t in grps[k] else k}' for t in n['held']}


def versions_of(names, parents):
    """[(song, [versions])] for training songs in their order, each song's copies (parents: copy -> its song) after it:
    the units that share one song's draws (kick_goal_nn.train) and weight (weights)."""
    out = {}
    for u in names:
        out.setdefault(parents.get(u, u), []).append(u)
    for t, vs in out.items():
        if vs[0] != t:
            raise ValueError(f'{t}: its copies train without it')
    return list(out.items())


def weights(ys, hards, clip, share=None, versions=None):
    """Row weights per song, the kick fast loop's: positives total .5, negatives .5 with vouched negatives HARD_W
    times as likely, so every song weighs the same. A song's versions (versions: its key per row set; a copy shares
    its song's key) split that one song's weight equally. With a share, the clip songs together weigh share of all."""
    from tools.audio_analysis.eval.kick_goal_melodic import HARD_W
    ws = []
    for y, hard in zip(ys, hards):
        neg = np.where(y == 0, np.where(hard, HARD_W, 1.0), 0.0)
        ws.append(np.where(y == 1, .5 / max(1, y.sum()), .5 * neg / max(1e-9, neg.sum())))
    if versions is not None:
        n = {v: list(versions).count(v) for v in versions}
        ws = [w / n[v] if n[v] > 1 else w for w, v in zip(ws, versions)]
    if share is not None and any(clip) and not all(clip):
        real = sum(w.sum() for w, c in zip(ws, clip) if not c)
        fake = sum(w.sum() for w, c in zip(ws, clip) if c)
        ws = [w * (real * share / (1 - share) / fake) if c else w for w, c in zip(ws, clip)]
    return ws


HARD_FLOOR_DB = 1.0   # a candidate that barely rises still gets drawn, at one dB's worth


def onset_strength(profile):
    """dB onset strength per candidate from its cached rise profile (components prof32, kick_goal_profile): the
    largest early-half rise over the 16 paired bands, floored at HARD_FLOOR_DB so every weight stays positive."""
    from tools.audio_analysis.eval.kick_goal_profile import PAIRS
    return np.maximum(np.asarray(profile, float)[:, :PAIRS].max(axis=1), HARD_FLOOR_DB)


def hard_negative_weights(strength, power):
    """Per-candidate draw weight of sampling.hard_negatives: onset strength to the power; train() normalises per song
    and multiplies it into the vouched-negative weight."""
    return np.asarray(strength, float) ** power


def coincident_row_weights(onset, y, positives, partner_positives, weight):
    """Per-candidate draw weight of sampling.coincident_weight: `weight` for a positive row whose nearest label of
    this song sits within the coincident window of a partner-family label (score.coincident_mask), 1 for every other
    row."""
    from tools.audio_analysis.lab.score import coincident_mask
    out = np.ones(len(onset))
    pos = np.sort(np.asarray(positives, float))
    if not len(pos) or not len(onset):
        return out
    onset = np.asarray(onset, float)
    j = np.searchsorted(pos, onset)
    lo, hi = np.clip(j - 1, 0, len(pos) - 1), np.clip(j, 0, len(pos) - 1)
    near = np.where(np.abs(onset - pos[lo]) <= np.abs(pos[hi] - onset), lo, hi)
    out[(np.asarray(y) == 1) & coincident_mask(pos, partner_positives)[near]] = weight
    return out


def fit_trees(p, rows, random_state, share=None, versions=None):
    """HistGradientBoosting on rows [(x, y, hard, is_clip)], the config's settings; versions as in weights."""
    from sklearn.ensemble import HistGradientBoostingClassifier
    from threadpoolctl import threadpool_limits
    w = weights([r[1] for r in rows], [r[2] for r in rows], [r[3] for r in rows], share, versions)
    x, y, w = np.concatenate([r[0] for r in rows]), np.concatenate([r[1] for r in rows]), np.concatenate(w)
    with threadpool_limits(TREE_THREADS, user_api='openmp'):
        return HistGradientBoostingClassifier(
            max_iter=p['max_iter'], learning_rate=p['learning_rate'], max_leaf_nodes=p['max_leaf_nodes'],
            min_samples_leaf=p['min_samples_leaf'], l2_regularization=p['l2'],
            random_state=random_state).fit(x, y, sample_weight=w * len(y))


def combine(how, layers):
    """The combined probability from the model layers {name: probabilities}; mean_logit is the shipped kick's blend."""
    from scipy.special import expit, logit
    if how != 'mean_logit':
        return layers[how]
    return expit(np.mean([logit(np.clip(layers[m], 1e-6, 1 - 1e-6)) for m in COMBINES[how]], axis=0))


def mean_logit_of(ps, clip=1e-6):
    """Several nets' probabilities as one (docs/DETECTOR_RUNTIME_DESIGN.md D17): expit of the mean clipped logit;
    one net's probabilities pass through untouched."""
    from scipy.special import expit, logit
    if len(ps) == 1:
        return ps[0]
    return expit(np.mean([logit(np.clip(p, clip, 1 - clip)) for p in ps], axis=0))


def final_layer(cfg):
    return 'combined' if len(COMBINES[cfg.combine]) > 1 else cfg.combine


def song_meta(s, c):
    """(labels, stamp, avail, sr, hop) the scorer judges one song by: a family with a tight hit window (labels' tol_s
    or window_s) is judged at the candidate, every other at its emission."""
    tight = 'tol_s' in s or 'window_s' in s
    return s, c.onset_s if tight else c.emit_s, c.avail, c.sr, c.hop


def cutoff(metas, probs):
    """The cutoff detector_eval.held_out would choose on these songs' probabilities: best pooled F1, never on
    recall-only or generated songs."""
    from tools.audio_analysis.eval.detector_eval import THRESHOLDS, choose_from, score
    use = [u for u in probs if not metas[u][0]['positives_only'] and metas[u][0].get('kind') != 'generated']
    sums = sum((np.array([score(*metas[u][:2], probs[u], metas[u][2], th, *metas[u][3:5]) for th in THRESHOLDS])
                for u in use), np.zeros((len(THRESHOLDS), 3), int))
    return choose_from([tuple(int(v) for v in row) for row in sums])
