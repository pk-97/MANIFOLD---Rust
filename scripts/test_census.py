#!/usr/bin/env python3
"""Record and compare a multiset of nextest test identities across crate moves.

Unit-test identities are nextest's test names (the module path within the
crate); package and binary IDs are discarded. Integration tests (binary-kind
`test`) use `<binary-name>::<test-name>`, where Cargo's binary name is the test
file stem. The folded `tests/main.rs` binary omits `main::`: its module `foo`
already supplies the same `foo::<path>` identity as standalone `tests/foo.rs`.
Custom Cargo target names are treated as stems and need an explicit --map if
renamed. Duplicate identities retain their counts, including ignored tests.
Diff maps apply to BEFORE only, once, using the longest matching old prefix.
"""

import argparse
from collections import Counter
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent


def identities(listing: dict) -> Counter:
    result = Counter()
    for suite in listing["rust-suites"].values():
        prefix = ""
        if suite["kind"] == "test" and suite["binary-name"] != "main":
            prefix = suite["binary-name"] + "::"
        result.update(prefix + name for name in suite["testcases"])
    return result


def prefix_map(value: str) -> tuple[str, str]:
    old, sep, new = value.partition("=")
    if not sep or not old:
        raise argparse.ArgumentTypeError("expected nonempty OLD_PREFIX=NEW_PREFIX")
    return old, new


def renamed(counts: Counter, maps: list[tuple[str, str]]) -> Counter:
    result = Counter()
    for name, count in counts.items():
        for old, new in sorted(maps, key=lambda pair: len(pair[0]), reverse=True):
            if name.startswith(old):
                name = new + name[len(old):]
                break
        result[name] += count
    return result


def read_census(path: Path) -> Counter:
    data = json.loads(path.read_text())
    if data["version"] != 1:
        raise ValueError(f"{path}: unsupported census version")
    counts = data["identities"]
    if not isinstance(counts, dict) or any(
        not isinstance(name, str) or type(count) is not int or count <= 0
        for name, count in counts.items()
    ):
        raise ValueError(f"{path}: invalid identity counts")
    return Counter(counts)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    record = commands.add_parser("record", help="list tests and write a census")
    record.add_argument("out", type=Path)
    record.add_argument("-p", "--package", action="append", default=[])
    record.add_argument("--features", action="append", default=[])
    diff = commands.add_parser("diff", help="print identity drift")
    diff.add_argument("before", type=Path)
    diff.add_argument("after", type=Path)
    diff.add_argument("--map", type=prefix_map, action="append", default=[])
    args = parser.parse_args(argv)
    try:
        if args.command == "record":
            command = ["cargo", "nextest", "list", "--message-format", "json",
                       "--manifest-path", str(ROOT / "Cargo.toml")]
            if args.package:
                for package in args.package:
                    command += ["-p", package]
            else:
                command += ["--workspace"]
            for feature in args.features:
                command += ["--features", feature]
            env = dict(os.environ, CARGO_BUILD_JOBS="4")
            listing = subprocess.run(command, capture_output=True, text=True, env=env)
            if listing.stderr:
                print(listing.stderr, file=sys.stderr, end="")
            if listing.returncode:
                return listing.returncode
            counts = identities(json.loads(listing.stdout))
            args.out.write_text(json.dumps(
                {"version": 1, "identities": dict(sorted(counts.items()))}, indent=2
            ) + "\n")
            print(f"Recorded {sum(counts.values())} tests ({len(counts)} identities) to {args.out}")
            return 0
        before = renamed(read_census(args.before), args.map)
        after = read_census(args.after)
        for name in sorted(before.keys() | after.keys()):
            delta = after[name] - before[name]
            if delta:
                print(f"{'appeared' if delta > 0 else 'vanished'} {name}: "
                      f"{abs(delta)} (before {before[name]}, after {after[name]})")
        if before == after:
            print(f"Census equal: {sum(after.values())} tests")
            return 0
        return 1
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(f"test-census: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
