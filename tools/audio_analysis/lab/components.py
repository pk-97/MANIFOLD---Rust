"""Detector parts: candidate finders and per-candidate measures, registered by name
so a config builds a detector from them. Every part reads only a song's audio and its candidates. Caches key on the
song's audio content, the finder and the part's code (lab/cache.py)."""
import importlib
from dataclasses import dataclass

import numpy as np

from tools.audio_analysis.lab.cache import artifact, code_id, module_id, spectrum


@dataclass(frozen=True)
class Candidates:
    cand: np.ndarray    # candidate hops
    avail: np.ndarray   # emission hops: when the decision for each candidate is made
    hop: int            # samples per hop
    sr: int

    @property
    def onset_s(self):
        return (self.cand + 1) * self.hop / self.sr

    @property
    def emit_s(self):
        return (self.avail + 1) * self.hop / self.sr


def _snare(rule, song, family):
    from tools.audio_analysis.eval import detector_cands, snare_cands
    db = float(rule.split(':')[1])
    return (dict(rule=rule, code=module_id(snare_cands, detector_cands)), lambda sr: 256,
            lambda x: snare_cands.candidates(x, song.sample_rate, db))


def _band(rule, song, family):
    from tools.audio_analysis.eval import detector_cands, detector_net
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    # The finder's own source, DEADLINE and its helpers' module: edits elsewhere in detector_net are no miss.
    code = module_id(detector_cands) + code_id(detector_net.family_candidates) + str(detector_net.DEADLINE)
    return (dict(rule=rule, code=code, band=fam.band), lambda sr: 256,
            lambda x: detector_net.family_candidates(rule, fam)(x, song.sample_rate, spectrum(song)))


def _kick(rule, song, family):
    from tools.audio_analysis.eval import kick_fusion_bandwise, kick_fusion_features
    return (dict(rule=rule, code=module_id(kick_fusion_bandwise, kick_fusion_features)),
            lambda sr: max(1, round(sr * 256 / 48_000)),
            lambda x: kick_fusion_bandwise.fusion_features(x, song.sample_rate)[:2])


# Rule kind -> (cache settings, hop for a sample rate, finder on audio).
FINDERS = {'snare': _snare, 'rise': _band, 'change': _band, 'union': _band, 'kick': _kick}


def candidates(song, rule, family, build=True):
    """Candidates for a finder rule: 'snare:DB', 'rise:DB' / 'change:DB' / 'union:R:C' in the family's band, or
    'kick' (the kick fusion finder). build=False: a miss is an error, so scoring never reads audio."""
    from tools.audio_analysis.lab.registry import mix
    kind = rule.split(':')[0]
    if kind not in FINDERS:
        raise ValueError(f'unknown candidate rule {rule}; known: {sorted(FINDERS)}')
    settings, hop, find = FINDERS[kind](rule, song, family)

    def make():
        if not build:
            raise SystemExit(f'{song.id}: no cached {rule} candidates; run the experiment first')
        return np.stack(find(mix(song))).astype(np.int64)
    a = artifact(song, 'candidates', settings, make)
    return Candidates(a[0], a[1], hop(song.sample_rate), song.sample_rate)


# Finders that read only the audio, so they run on a bare clip (lab/release.py goldens).
AUDIO_FINDERS = ('kick', 'snare')


def audio_candidates(rule, family, x, sr):
    """Candidates of an audio-only finder rule on audio x at sr, the stream starting at x[0]: the registry's own
    finder closure, so the release reference and training share one finder. The training cache (candidates()) keeps
    calling the closure directly, so its code key, and every trained layer keyed on it, is unchanged."""
    import types
    kind = rule.split(':')[0]
    if kind not in AUDIO_FINDERS:
        raise ValueError(f'finder {rule} reads more than the audio; audio-only finders: {AUDIO_FINDERS}')
    _, hop, fn = FINDERS[kind](rule, types.SimpleNamespace(sample_rate=sr), family)
    cand, avail = (np.asarray(a, np.int64) for a in fn(x))
    return Candidates(cand, avail, hop(sr), sr)


def finder_id(song, rule, family):
    """What decides a song's candidates: measures computed on them key on it."""
    settings = FINDERS[rule.split(':')[0]](rule, song, family)[0]
    return '|'.join(f'{k}={v}' for k, v in sorted(settings.items()))


# Event measure builders, f(audio, sr, Candidates) -> (n, k). Each is causal and never fitted.
def _f15(x, sr, c):
    from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features
    cand, avail, f, hop = fusion_features(x, sr)
    if not (np.array_equal(cand, c.cand) and np.array_equal(avail, c.avail) and hop == c.hop):
        raise ValueError('f15 comes with the kick finder: use it with candidates "kick"')
    return f


def _tail3(x, sr, c):
    from tools.audio_analysis.eval.kick_goal_tail_features import tail_features
    return tail_features(x, sr, c.onset_s, c.emit_s)


def _prof32(x, sr, c):
    from tools.audio_analysis.eval.kick_goal_profile import rise_profile
    from tools.audio_analysis.eval.kick_goal_templates import band_spec
    return rise_profile(band_spec(x, sr, c.hop), c.cand, c.avail)


def _fall16(x, sr, c):
    from tools.audio_analysis.eval.kick_goal_profile import fall_profile
    from tools.audio_analysis.eval.kick_goal_templates import band_spec
    return fall_profile(band_spec(x, sr, c.hop), c.cand, c.avail)


def _low16(x, sr, c):
    from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes, lowbank_features
    return lowbank_features(band_envelopes(x, sr), c.onset_s, c.emit_s)


def _res16(x, sr, c):
    from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes
    from tools.audio_analysis.eval.kick_goal_residual import residual_features
    return residual_features(band_envelopes(x, sr).astype(np.float32).astype(np.float64), c.onset_s, c.emit_s)


def _glide3(x, sr, c):
    from tools.audio_analysis.eval.kick_glide_features import glide_features
    return glide_features(x, sr, c.hop, c.cand, c.avail)


def _pitch6(x, sr, c):
    """Pitch stability (detector_channels) in 300-1k, 1-3k and 3-8 kHz, mean and max from onset to emission: a held
    pitch (piano, synth, voice) sits near 1, a noise hit near 0."""
    from tools.audio_analysis.eval.detector_channels import pitch_measures, pitch_stability
    return pitch_measures(pitch_stability(x, sr), c.cand, c.avail, sr, c.hop)


def _note13(x, sr, c):
    """note_pitch's tonal-onset set: low f0 before and after and its change, YIN dips, low levels, chroma change and
    entropy, mid-band level change and the share of new spectral peaks."""
    from tools.audio_analysis.eval.note_pitch import pitch_features
    return pitch_features(x, sr, c.onset_s, c.emit_s)


CDOM_PRE_HOPS = 10     # hops before the onset the pre-onset means read
CDOM_RAW_EPS = 1e-9    # log10(raw + eps)


def _cdom4(x, sr, c):
    """Complex-domain prediction error (45-2000 Hz; kick_complex_onset): max of the normalised and the log raw error
    from onset to emission, and their means over the 10 hops before the onset. A steady tone is predictable (low),
    a noise burst is not."""
    from tools.audio_analysis.eval.kick_complex_onset import complex_features
    f = complex_features(x, sr, c.hop)
    nrm, raw = f['normalized'], np.log10(f['raw'] + CDOM_RAW_EPS)
    out = np.zeros((len(c.cand), 4))
    last = len(nrm) - 1
    for n, (i, j) in enumerate(zip(np.clip(c.cand, 0, last), np.clip(c.avail, 0, last))):
        lo = max(0, i - CDOM_PRE_HOPS)
        out[n] = (nrm[i:j + 1].max(), raw[i:j + 1].max(), nrm[lo:i].mean() if i > lo else 0.0,
                  raw[lo:i].mean() if i > lo else raw[i])
    return out


# Event measures: (n candidates, k) numbers at each candidate, for trees.
MEASURES = {'f15': _f15, 'tail3': _tail3, 'prof32': _prof32, 'fall16': _fall16, 'low16': _low16, 'res16': _res16,
            'glide3': _glide3, 'pitch6': _pitch6, 'note13': _note13, 'cdom4': _cdom4}


def _kick2(song, c):
    from tools.audio_analysis.lab import kick_verdict
    return kick_verdict.features(song, c)


# Event measures that read another detector's stored verdict for the song (lab/kick_verdict.py), not the audio:
# measure() builds them from the song; a bare clip has no verdict, so a release cannot use them until the app runs
# the kick before the snare.
SONG_MEASURES = {'kick2': _kick2}
MEASURES['kick2'] = _kick2


def _flat(song):
    from tools.audio_analysis.lab.cache import flatness
    return flatness(song)


def _perc(song):
    from tools.audio_analysis.lab.cache import percussive
    return percussive(song)


# Frame measures: (frames, bands) on the net's 2 ms grid, added to the net as input channels.
FRAME_MEASURES = {'flat': _flat, 'perc': _perc}


def _flat_audio(x, sr):
    from tools.audio_analysis.eval.detector_channels import flatness
    return flatness(x, sr)


# The pure builders FRAME_MEASURES cache (lab/cache.py stores their float16 output): f(audio, sr) -> (frames, bands)
# float32, for a bare clip.
def _perc_audio(x, sr):
    from tools.audio_analysis.eval.kick_goal_nn import percussive, spectrum
    return percussive(spectrum(x, sr)).astype(np.float32)


FRAME_BUILDERS = {'flat': _flat_audio, 'perc': _perc_audio}


def _tmpl3_fit(inputs, clips, k, random_state, share=None):
    from tools.audio_analysis.lab import templates
    return dict(templates=templates.fit(inputs, clips, k, random_state, share))


def _tmpl3_apply(params, x, sr, c):
    from tools.audio_analysis.lab import templates
    return templates.features(templates._build(x, sr, c), params['templates'])


# Fitted event measures: fit(train inputs) -> {key: array} at release, then
# apply(params, audio, sr, Candidates) -> (n, k). Their params ship in the model file as fit.<measure>.<key>.
FITTED = {'tmpl3': dict(fit=_tmpl3_fit, apply=_tmpl3_apply)}


def measure(song, rule, family, name, build=True):
    """(n candidates, k) event measure for a song's candidates under a finder rule."""
    from tools.audio_analysis.lab.registry import mix
    if name not in MEASURES:
        raise ValueError(f'unknown measure {name}; known: {sorted(MEASURES)}')
    fn = MEASURES[name]
    extra = {}
    if name in SONG_MEASURES:
        from tools.audio_analysis.lab import kick_verdict
        extra = dict(verdict=kick_verdict.kick_release())

    def make():
        if not build:
            raise SystemExit(f'{song.id}: no cached {name} measure; run the experiment first')
        c = candidates(song, rule, family)
        if name in SONG_MEASURES:
            return np.asarray(fn(song, c), np.float64)
        return np.asarray(fn(mix(song), song.sample_rate, c), np.float64)
    return artifact(song, f'measure-{name}', dict(finder=finder_id(song, rule, family), code=code_id(fn), **extra), make)


def frame(song, name):
    """(frames, bands) frame measure for the net."""
    return FRAME_MEASURES[name](song)
