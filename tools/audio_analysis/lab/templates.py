"""Sound templates for trees (docs/DETECTOR_LAB_DESIGN.md P6): the shipped kick's tmpl3 (eval/kick_goal_templates.py)
as a fitted tree input for any family. A candidate's onset patch is the rise across 32 log bands from the candidate to
its emission, from the song's mix; patches are cached per song by audio content, finder and code, and never fitted.
Each training fits its own K templates (k-means of L2-normalised patches) on the positive training rows of the songs
and clips it trains on, never on its held songs. Every song it trains on or predicts then gets three numbers per
candidate: the best cosine over templates for the whole patch, its bands under 300 Hz and its bands over 1 kHz."""
import numpy as np

# Patches per song in one fit, drawn at random. A real kick song has 420 kick rows at the median and up to 2084;
# 200 keeps a fit near 15k patches, and the song weights below keep every song's say equal whatever its count.
CAP = 200


def _build(x, sr, c):
    from tools.audio_analysis.eval.kick_goal_templates import BANDS, band_spec, patches
    if not len(c.cand):
        return np.zeros((0, 1, BANDS))
    return patches(band_spec(x, sr, c.hop), c.cand, c.avail)


def patches(song, rule, family, build=True):
    """(n candidates, span, 32) onset patches for a song's candidates under a finder rule."""
    from tools.audio_analysis.lab import components
    from tools.audio_analysis.lab.cache import artifact, code_id
    from tools.audio_analysis.lab.registry import mix

    def make():
        if not build:
            raise SystemExit(f'{song.id}: no cached patches; run the experiment first')
        return np.asarray(_build(mix(song), song.sample_rate, components.candidates(song, rule, family)), np.float64)
    return artifact(song, 'patches', dict(finder=components.finder_id(song, rule, family), code=code_id(_build)), make)


def song_inputs(cfg, lab, songs, build=True):
    """{song: (patches, training rows (mask, y, hard))} for every song of a run."""
    from tools.audio_analysis.eval.detector_eval import rows
    from tools.audio_analysis.lab import components
    out = {}
    for t, s in lab.items():
        c = components.candidates(songs[t], cfg.candidates, cfg.family, build)
        p = patches(songs[t], cfg.candidates, cfg.family, build)
        if len(p) != len(c.cand):
            raise ValueError(f'{t}: {len(p)} patches for {len(c.cand)} candidates')
        out[t] = (p, rows(s, c.onset_s))
    return out


def span(inputs):
    """The one patch length every song shares; templates need it."""
    spans = {p.shape[1] for p, _ in inputs.values() if len(p)}
    if len(spans) != 1:
        raise ValueError(f'templates need one patch span across songs (candidate to emission); got {sorted(spans)}')
    return spans.pop()


def _kmeans(x, w, k, random_state):
    from sklearn.cluster import KMeans
    from threadpoolctl import threadpool_limits

    from tools.audio_analysis.lab.models import TREE_THREADS
    with threadpool_limits(TREE_THREADS, user_api='openmp'):
        return KMeans(n_clusters=k, n_init=4, random_state=random_state).fit(x, sample_weight=w).cluster_centers_


def fit(inputs, clips, k, random_state, share=None):
    """(k, span, 32) templates from the positive training rows of the given songs only (the training's trained songs
    and clips). At most CAP patches per song; weighted like tree rows (each song equal, clips together `share` of the
    weight when set); kick_goal_templates.fit_templates otherwise: empty patches dropped, k-means on unit patches."""
    from tools.audio_analysis.eval.kick_goal_templates import BANDS, unit
    from tools.audio_analysis.lab.models import weights
    rng = np.random.default_rng(random_state)
    ps, is_clip = [], []
    for u, (p, (mask, y, _)) in inputs.items():
        p = p[mask & (y == 1)]
        p = p[p.sum(axis=(1, 2)) > 0]
        if len(p) > CAP:
            p = p[np.sort(rng.choice(len(p), CAP, replace=False))]
        if len(p):
            ps.append(p)
            is_clip.append(u in clips)
    n = sum(len(p) for p in ps)
    if n < k:
        raise ValueError(f'{n} positive patches for {k} templates')
    w = weights([np.ones(len(p)) for p in ps], [np.zeros(len(p), bool) for p in ps], is_clip, share)
    x = np.concatenate(ps)
    return _kmeans(unit(x), np.concatenate(w), k, random_state).reshape(k, x.shape[1], BANDS)


def features(p, templates):
    """(n, 3): kick_goal_templates.template_features of a song's patches against the templates."""
    from tools.audio_analysis.eval.kick_goal_templates import template_features
    return template_features(p, templates) if len(p) else np.zeros((0, 3))
