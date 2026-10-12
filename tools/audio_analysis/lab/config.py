"""Experiment configs (docs/DETECTOR_LAB_DESIGN.md D6, D9, P6): one JSON file per experiment in lab/experiments/;
an A/B is two configs. A config builds a detector from parts (lab/components.py): a candidate finder, models (net,
trees or both), how they combine, and optionally the song-history stage (lab/stage.py). Every key is required and
no other key is allowed, so a typo is an error, not a silent default. Schema 1 configs (one net, a 'flat' switch) migrate on read,
and a schema 2 net without the training options reads them as NET_DEFAULTS (today's training); trees without
'templates' read it as null (no sound templates: today's trees)."""
import json
from dataclasses import asdict, dataclass
from pathlib import Path

EXPERIMENTS = Path(__file__).with_name('experiments')
SCHEMA = 2
KEYS = {'family', 'candidates', 'models', 'combine', 'stage', 'sampling', 'train', 'seeds', 'folds'}
# swa_epochs: average the weights of the last N epochs (0 off); precision: 'float32' or 'mixed' (float16 autocast).
NET_DEFAULTS = dict(swa_epochs=0, batch=256, lr=0.002, precision='float32')
NET_KEYS = {'past_ms', 'ahead_ms', 'channels', 'epochs', *NET_DEFAULTS}
PRECISIONS = ('float32', 'mixed')
# Sampling options that are absent when off (a null reads as absent, so the key and run ids stay as before).
OPTIONAL_SAMPLING = ('gain_variants', 'gain_probes', 'hard_negatives', 'coincident_weight')
# Families with a partner family's labels for sampling.coincident_weight (score.PARTNER; a test keeps them equal).
COINCIDENT_FAMILIES = ('snare', 'kick')
# templates: null, or {'k': K} sound templates fitted per training (lab/templates.py), three tree inputs.
TREE_DEFAULTS = dict(templates=None)
TREE_KEYS = {'measures', 'max_iter', 'learning_rate', 'max_leaf_nodes', 'min_samples_leaf', 'l2', *TREE_DEFAULTS}
COMBINES = {'net': ('net',), 'trees': ('trees',), 'mean_logit': ('net', 'trees')}


@dataclass(frozen=True)
class Experiment:
    name: str
    family: str
    candidates: str    # a finder rule (components.FINDERS)
    models: dict       # {'net': {past_ms, ahead_ms, channels, epochs, NET_DEFAULTS keys}, 'trees': {...}}: one or both
    combine: str       # 'net' | 'trees' | 'mean_logit' (both)
    stage: dict | None  # the song-history stage (lab/stage.py STAGE_KEYS) or None
    sampling: dict     # per_song 'median'; generated 'songs' (ordinary songs) or a share
    train: tuple       # ('project',) or ('project', 'generated')
    seeds: tuple
    folds: int         # 0: one net per song; N: N fixed song groups

    @property
    def net(self):
        return self.models.get('net')

    @property
    def ahead_s(self):
        """The net's look past the emission: the decision waits this long."""
        return self.net['ahead_ms'] / 1000 if self.net else 0.0

    def net_env(self):
        """The kick net's input env; kick_goal_nn reads it at import. Channels are passed to the net, not env."""
        return dict(KICK_GOAL_NN_PAST_MS=str(self.net['past_ms'] if self.net else 150), KICK_GOAL_NN_AHEAD_MS='0',
                    KICK_GOAL_NN_PERC='0', KICK_GOAL_NN_FLAT='0')

    @property
    def gain_variants(self):
        """None, or {'count': K, 'db': [lo, hi]}: K extra copies of each exam song at fixed seeded gains, training only."""
        return self.sampling.get('gain_variants')

    @property
    def gain_probes(self):
        """None, or dB gains: each exam song also predicted (held out, never trained on) at these gains, scored by
        score.py as the level-robustness section."""
        return self.sampling.get('gain_probes')

    @property
    def hard_negatives(self):
        """None, or {'by': 'onset', 'power': P}: the net draws each negative candidate in proportion to its onset
        strength to the power P (per song, times the vouched-negative weight)."""
        return self.sampling.get('hard_negatives')

    @property
    def coincident_weight(self):
        """None, or W: the net draws a positive whose label sits within 20 ms of a partner-family label (snare <->
        kick) W times as often."""
        return self.sampling.get('coincident_weight')

    def generated_as_clips(self):
        return 'generated' in self.train and self.sampling['generated'] != 'songs'

    def key(self):
        """What decides a run's outputs; the name and the seed list do not."""
        d = asdict(self)
        del d['name'], d['seeds']
        return d


def migrate(d):
    """A schema 1 config dict (one net with a 'flat' switch) as schema 2; a net without the training options gets
    NET_DEFAULTS, trees without templates TREE_DEFAULTS."""
    d = dict(d)
    if 'net' in d:
        net = d.pop('net')
        d['models'] = dict(net=dict(past_ms=net['past_ms'], ahead_ms=net['ahead_ms'],
                                    channels=['flat'] if net.get('flat') else [], epochs=net['epochs']))
        d['combine'], d['stage'] = 'net', None
    if 'net' in d.get('models', {}):
        d['models'] = dict(d['models'], net=NET_DEFAULTS | d['models']['net'])
    if 'trees' in d.get('models', {}):
        d['models'] = dict(d['models'], trees=TREE_DEFAULTS | d['models']['trees'])
    return d


def check(d, where=''):
    from tools.audio_analysis.lab.components import FRAME_MEASURES, MEASURES
    if set(d) != KEYS:
        raise ValueError(f'{where}: keys must be exactly {sorted(KEYS)}')
    m = d['models']
    if not m or set(m) - {'net', 'trees'}:
        raise ValueError(f'{where}: models holds "net", "trees" or both')
    if 'net' in m and (set(m['net']) != NET_KEYS or set(m['net']['channels']) - set(FRAME_MEASURES)):
        raise ValueError(f'{where}: net keys are {sorted(NET_KEYS)}, channels from {sorted(FRAME_MEASURES)}')
    if 'net' in m:
        n = m['net']
        if not (isinstance(n['swa_epochs'], int) and 0 <= n['swa_epochs'] <= n['epochs'] and isinstance(n['batch'], int)
                and n['batch'] > 0 and n['lr'] > 0 and n['precision'] in PRECISIONS):
            raise ValueError(f'{where}: net swa_epochs in 0..epochs, batch > 0, lr > 0, precision in {PRECISIONS}')
    if 'trees' in m and (set(m['trees']) != TREE_KEYS or not m['trees']['measures']
                         or set(m['trees']['measures']) - set(MEASURES)):
        raise ValueError(f'{where}: trees keys are {sorted(TREE_KEYS)}, measures from {sorted(MEASURES)}')
    tm = m.get('trees', {}).get('templates')
    if tm is not None and not (isinstance(tm, dict) and set(tm) == {'k'} and type(tm['k']) is int and tm['k'] >= 2):
        raise ValueError(f'{where}: trees templates is null or {{"k": K}} with an integer K >= 2')
    if COMBINES.get(d['combine']) != tuple(sorted(m)):
        raise ValueError(f'{where}: combine {d["combine"]!r} does not match models {sorted(m)}')
    if d['stage'] is not None:
        from tools.audio_analysis.lab.stage import check as check_stage
        check_stage(d['stage'], int(d['folds']), MEASURES, where)
    train = tuple(d['train'])
    if train not in (('project',), ('project', 'generated')):
        raise ValueError(f'{where}: train is ["project"] or ["project", "generated"]')
    s = d['sampling']
    if s.get('per_song') != 'median':
        raise ValueError(f'{where}: sampling.per_song is "median" in v1 (label-rich draws: BUG-9ngk8.5 (label-rich songs))')
    gen = s.get('generated')
    if ('generated' in train) != (gen is not None) or set(s) - {'per_song', 'generated', *OPTIONAL_SAMPLING}:
        raise ValueError(f'{where}: sampling.generated is set exactly when generated songs train')
    if gen is not None and gen != 'songs' and not (isinstance(gen, float) and 0 < gen < 1):
        raise ValueError(f'{where}: sampling.generated is "songs" or a share between 0 and 1')
    gv = s.get('gain_variants')
    if gv is not None and not (isinstance(gv, dict) and set(gv) == {'count', 'db'} and type(gv['count']) is int
                               and gv['count'] >= 1 and isinstance(gv['db'], list) and len(gv['db']) == 2
                               and all(isinstance(v, (int, float)) for v in gv['db']) and gv['db'][0] <= gv['db'][1]):
        raise ValueError(f'{where}: sampling.gain_variants is null or {{"count": K >= 1, "db": [lo, hi]}}')
    gp = s.get('gain_probes')
    if gp is not None and not (isinstance(gp, list) and gp and all(isinstance(v, (int, float)) and v for v in gp)):
        raise ValueError(f'{where}: sampling.gain_probes is null or a list of non-zero dB gains')
    hn = s.get('hard_negatives')
    if hn is not None and not (isinstance(hn, dict) and set(hn) == {'by', 'power'} and hn['by'] == 'onset'
                               and type(hn['power']) in (int, float) and hn['power'] > 0 and 'net' in m):
        raise ValueError(f'{where}: sampling.hard_negatives is null or {{"by": "onset", "power": P > 0}}, and needs a net')
    cw = s.get('coincident_weight')
    if cw is not None and not (type(cw) in (int, float) and cw > 0 and 'net' in m and d['family'] in COINCIDENT_FAMILIES):
        raise ValueError(f'{where}: sampling.coincident_weight is null or W > 0, for a net in the {COINCIDENT_FAMILIES} families')


def build(name, d):
    """An Experiment from a config dict (schema 1 migrates); seeds may be absent (a run's meta drops them)."""
    d = migrate(d)
    if 'sampling' in d:   # null is absent: the key (and run ids) stay as before
        d = dict(d, sampling={k: v for k, v in d['sampling'].items() if k not in OPTIONAL_SAMPLING or v is not None})
    check({k: v for k, v in d.items() if k != 'seeds'} | {'seeds': d.get('seeds', [])}, name)
    return Experiment(name=name, family=d['family'], candidates=d['candidates'], models=d['models'],
                      combine=d['combine'], stage=d['stage'], sampling=d['sampling'], train=tuple(d['train']),
                      seeds=tuple(d.get('seeds', ())), folds=int(d['folds']))


def load(name_or_path):
    p = Path(name_or_path)
    if not p.exists():
        p = EXPERIMENTS / f'{name_or_path}.json'
    return build(p.stem, json.loads(p.read_text()))
