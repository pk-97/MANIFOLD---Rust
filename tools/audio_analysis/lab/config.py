"""Experiment configs (docs/DETECTOR_LAB_DESIGN.md D6, D9, P6): one JSON file per experiment in lab/experiments/;
an A/B is two configs. A config builds a detector from parts (lab/components.py): a candidate finder, models (net,
trees or both), how they combine, and later the song-memory stage. Every key is required and no other key is
allowed, so a typo is an error, not a silent default. Schema 1 configs (one net, a 'flat' switch) migrate on read."""
import json
from dataclasses import asdict, dataclass
from pathlib import Path

EXPERIMENTS = Path(__file__).with_name('experiments')
SCHEMA = 2
KEYS = {'family', 'candidates', 'models', 'combine', 'stage', 'sampling', 'train', 'seeds', 'folds'}
NET_KEYS = {'past_ms', 'ahead_ms', 'channels', 'epochs'}
TREE_KEYS = {'measures', 'max_iter', 'learning_rate', 'max_leaf_nodes', 'min_samples_leaf', 'l2'}
COMBINES = {'net': ('net',), 'trees': ('trees',), 'mean_logit': ('net', 'trees')}


@dataclass(frozen=True)
class Experiment:
    name: str
    family: str
    candidates: str    # a finder rule (components.FINDERS)
    models: dict       # {'net': {past_ms, ahead_ms, channels, epochs}, 'trees': {measures, ...}}: one or both
    combine: str       # 'net' | 'trees' | 'mean_logit' (both)
    stage: dict | None  # the song-memory stage (P6 second pass)
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

    def generated_as_clips(self):
        return 'generated' in self.train and self.sampling['generated'] != 'songs'

    def key(self):
        """What decides a run's outputs; the name and the seed list do not."""
        d = asdict(self)
        del d['name'], d['seeds']
        return d


def migrate(d):
    """A schema 1 config dict (one net with a 'flat' switch) as schema 2."""
    if 'net' not in d:
        return d
    d = dict(d)
    net = d.pop('net')
    d['models'] = dict(net=dict(past_ms=net['past_ms'], ahead_ms=net['ahead_ms'],
                                channels=['flat'] if net.get('flat') else [], epochs=net['epochs']))
    d['combine'], d['stage'] = 'net', None
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
    if 'trees' in m and (set(m['trees']) != TREE_KEYS or not m['trees']['measures']
                         or set(m['trees']['measures']) - set(MEASURES)):
        raise ValueError(f'{where}: trees keys are {sorted(TREE_KEYS)}, measures from {sorted(MEASURES)}')
    if COMBINES.get(d['combine']) != tuple(sorted(m)):
        raise ValueError(f'{where}: combine {d["combine"]!r} does not match models {sorted(m)}')
    if d['stage'] is not None:
        raise ValueError(f'{where}: the stage arrives in the P6 second pass')
    train = tuple(d['train'])
    if train not in (('project',), ('project', 'generated')):
        raise ValueError(f'{where}: train is ["project"] or ["project", "generated"]')
    s = d['sampling']
    if s.get('per_song') != 'median':
        raise ValueError(f'{where}: sampling.per_song is "median" in v1 (label-rich draws: BUG-9ngk8.5 (label-rich songs))')
    gen = s.get('generated')
    if ('generated' in train) != (gen is not None) or set(s) - {'per_song', 'generated'}:
        raise ValueError(f'{where}: sampling.generated is set exactly when generated songs train')
    if gen is not None and gen != 'songs' and not (isinstance(gen, float) and 0 < gen < 1):
        raise ValueError(f'{where}: sampling.generated is "songs" or a share between 0 and 1')


def build(name, d):
    """An Experiment from a config dict (schema 1 migrates); seeds may be absent (a run's meta drops them)."""
    d = migrate(d)
    check({k: v for k, v in d.items() if k != 'seeds'} | {'seeds': d.get('seeds', [])}, name)
    return Experiment(name=name, family=d['family'], candidates=d['candidates'], models=d['models'],
                      combine=d['combine'], stage=d['stage'], sampling=d['sampling'], train=tuple(d['train']),
                      seeds=tuple(d.get('seeds', ())), folds=int(d['folds']))


def load(name_or_path):
    p = Path(name_or_path)
    if not p.exists():
        p = EXPERIMENTS / f'{name_or_path}.json'
    return build(p.stem, json.loads(p.read_text()))
