#!/usr/bin/env python3
"""Nightly compile/lint coverage for every non-default Cargo feature."""
import argparse
import os
from pathlib import Path
import subprocess
import time

from gate_workspace import Workspace

REPO = Path(__file__).resolve().parent.parent


def workspace_features():
    return Workspace(REPO).feature_matrix()


def check_coverage():
    Workspace(REPO)  # Failure is red; there is no partial matrix.
    return []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--list', action='store_true')
    parser.add_argument('--check-coverage', action='store_true')
    args = parser.parse_args()
    try:
        matrix = workspace_features()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f'[FAIL] feature metadata: {error}')
        return 1
    if args.list:
        for package, feature in matrix:
            print(f'{package} --features {feature}')
        return 0
    if args.check_coverage:
        print(f'[PASS] feature coverage: {len(matrix)} metadata-derived build rows')
        return 0
    failed = []
    for package, feature in matrix:
        command = ['cargo', 'clippy', '--manifest-path', str(REPO / 'Cargo.toml'),
                   '-p', package, '--features', feature, '--tests', '--', '-D', 'warnings']
        start = time.monotonic()
        result = subprocess.run(command, cwd=REPO, capture_output=True, text=True,
                                env=dict(os.environ, CARGO_BUILD_JOBS='4', CARGO_INCREMENTAL='0'))
        status = 'PASS' if result.returncode == 0 else 'FAIL'
        print(f'[{status}] {package} --features {feature} ({time.monotonic() - start:.0f}s)')
        if result.returncode:
            failed.append((package, feature))
            print('\n'.join((result.stdout + result.stderr).splitlines()[-25:]))
    return int(bool(failed))


if __name__ == '__main__':
    raise SystemExit(main())
