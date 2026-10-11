"""The held-out nets every detector family trains through the Detector Lab (lab/run.py): candidate finders for the
melodic families, and the song groups and held-out training loop around kick_goal_nn.

Candidates: 'rise:DB' (the family band's energy rises DB over the last 4 hops' minimum) or 'change:DB' (net bands in
the family range rise DB over their own last-20 ms mean; detector_cands.spectral_rise), or 'union:RISE:CHANGE'."""

import numpy as np

DEADLINE = 8  # hops from a candidate to its emission, every finder below


def family_candidates(spec_name, fam):
    """f(x, sr, spec) -> (candidate hops, available hops) for a CANDS argument."""
    from tools.audio_analysis.eval.detector_cands import band_hops, peak_candidates, rise_candidates, spectral_rise
    kind, *args = spec_name.split(':')
    a = [float(v) for v in args]

    def rise(x, sr, spec):
        return rise_candidates(band_hops(x, sr, *fam.band), a[0], 4, 6, DEADLINE)

    def change(x, sr, spec, thr=None):
        return peak_candidates(spectral_rise(spec, sr, *fam.band), a[-1] if thr is None else thr, 6, DEADLINE)

    def union(x, sr, spec):
        c = np.unique(np.concatenate([rise(x, sr, spec)[0], change(x, sr, spec, a[1])[0]]))
        keep, last = [], -100
        for h in c:
            if h - last >= 6:
                keep.append(h)
                last = h
        c = np.array(keep, int)
        return c, c + DEADLINE
    return {'rise': rise, 'change': change, 'union': union}[kind]


def groups(songs, folds):
    """Fixed song groups: names sorted, dealt round-robin, so the same song set always splits the same way. folds 0:
    one group per song in the labels' order (the seeds of the leave-one-song-out runs depend on it)."""
    if not folds:
        return [[t] for t in songs]
    names = sorted(songs)
    return [names[k::folds] for k in range(folds)]


def net_groups(songs, folds, untrained=()):
    """The song groups one run's nets hold out (groups). With one net per song, the untrained songs share one last
    net: every net would train on the same songs for them."""
    if folds or not untrained:
        return groups(songs, folds)
    return groups([t for t in songs if t not in untrained], 0) + [[t for t in songs if t in untrained]]
