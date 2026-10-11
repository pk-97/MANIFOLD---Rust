"""Experiment configs (docs/DETECTOR_LAB_DESIGN.md D6, D9): one JSON file per experiment in lab/experiments/; an A/B
is two configs. Every key is required and no other key is allowed, so a typo is an error, not a silent default."""
import json
from dataclasses import asdict, dataclass
from pathlib import Path

EXPERIMENTS = Path(__file__).with_name('experiments')
KEYS = {'family', 'candidates', 'net', 'sampling', 'train', 'seeds', 'folds'}
NET_KEYS = {'past_ms', 'ahead_ms', 'flat', 'epochs'}


@dataclass(frozen=True)
class Experiment:
    name: str
    family: str
    candidates: str    # 'snare:DB'
    net: dict          # past_ms (slice), ahead_ms (look past emission), flat (noisiness channel), epochs
    sampling: dict     # per_song 'median'; generated 'songs' (ordinary songs) or a share
    train: tuple       # ('project',) or ('project', 'generated')
    seeds: tuple
    folds: int         # 0: one net per song; N: N fixed song groups

    def net_env(self):
        """The kick net's input env; kick_goal_nn reads it at import."""
        return dict(KICK_GOAL_NN_PAST_MS=str(self.net['past_ms']), KICK_GOAL_NN_AHEAD_MS='0',
                    KICK_GOAL_NN_PERC='1' if self.net['flat'] else '0', KICK_GOAL_NN_FLAT='0')

    def generated_as_clips(self):
        return 'generated' in self.train and self.sampling['generated'] != 'songs'

    def key(self):
        """What decides a run's outputs; the name and the seed list do not."""
        d = asdict(self)
        del d['name'], d['seeds']
        return d


def load(name_or_path):
    p = Path(name_or_path)
    if not p.exists():
        p = EXPERIMENTS / f'{name_or_path}.json'
    d = json.loads(p.read_text())
    if set(d) != KEYS or set(d['net']) != NET_KEYS:
        raise ValueError(f'{p}: keys must be exactly {sorted(KEYS)}, net {sorted(NET_KEYS)}')
    train = tuple(d['train'])
    if train not in (('project',), ('project', 'generated')):
        raise ValueError(f'{p}: train is ["project"] or ["project", "generated"]')
    s = d['sampling']
    if s.get('per_song') != 'median':
        raise ValueError(f'{p}: sampling.per_song is "median" in v1 (label-rich draws: BUG-9ngk8.5 (label-rich songs))')
    gen = s.get('generated')
    if ('generated' in train) != (gen is not None) or set(s) - {'per_song', 'generated'}:
        raise ValueError(f'{p}: sampling.generated is set exactly when generated songs train')
    if gen is not None and gen != 'songs' and not (isinstance(gen, float) and 0 < gen < 1):
        raise ValueError(f'{p}: sampling.generated is "songs" or a share between 0 and 1')
    return Experiment(name=p.stem, family=d['family'], candidates=d['candidates'], net=d['net'], sampling=s,
                      train=train, seeds=tuple(d['seeds']), folds=int(d['folds']))
