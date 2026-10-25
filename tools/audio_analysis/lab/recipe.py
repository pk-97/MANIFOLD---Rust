"""The pinned environment of every lab process.

SONG_ENV decides which songs the kick goal set holds and how their truth is read (kick_goal_eval reads it at
import). A lab run adds the net's input env from its config; the snare release adds its own. apply() must run before
any eval/ module is imported, and exits on a conflicting value already in the environment.
"""
import os
import sys

SONG_ENV = dict(KICK_GOAL_TRUTH='v3', KICK_GOAL_MORE='1', KICK_GOAL_TRIGGER='1', KICK_GOAL_WIP='1',
                KICK_GOAL_PROJECT='1', KICK_GOAL_RECALL='1')
MUST_BE_UNSET = ('KICK_GOAL_NN_BANDS', 'KICK_GOAL_NN_STEREO', 'SNARE_AHEAD_MS', 'KICK_GOAL_WIP2')
THREADS = ('OMP_NUM_THREADS', 'OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'MKL_NUM_THREADS')


def apply(net_env=None):
    for k, v in {**SONG_ENV, **(net_env or {})}.items():
        if os.environ.get(k, v) != v:
            sys.exit(f'{k}={os.environ[k]} conflicts with the pinned recipe ({v})')
        os.environ[k] = v
    for k in MUST_BE_UNSET:
        if os.environ.get(k):
            sys.exit(f'{k} is set; the pinned recipe uses the default')
    for k in THREADS:
        os.environ.setdefault(k, '1')
