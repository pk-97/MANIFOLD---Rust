#!/usr/bin/env python3
"""Move-identity verifier — the pure-move gate for the god-file decomposition waves.

A "pure move" commit relocates code without changing it. This script proves it
mechanically: it runs `git diff --color-moved` with pinned colors and counts
every added/removed line that git did NOT classify as moved. For a pure-move
commit the non-moved residue must be ZERO after the allowlist (module wiring:
`mod`/`use`/`pub use` lines, blank lines, module doc comments, diff headers).
Exit code 0 = pure move proven; 1 = residue found (printed); 2 = usage.

Why not `cargo public-api`: not installed, requires a lib target (manifold-app
is bin-only — most of Wave 1 is invisible to it), and moving a pub item across
modules legitimately changes its path. This script sees every line instead.

Usage:
  python3 scripts/move_identity_check.py <commit>            # one commit vs its parent
  python3 scripts/move_identity_check.py <base>..<head>      # a range
  python3 scripts/move_identity_check.py --cached            # staged changes
  python3 scripts/move_identity_check.py <ref> --show-all    # print all residue lines

The moved-line detection uses `--color-moved=plain` with `--color-moved-ws=
ignore-all-space` so re-indented relocations still count as moves, and pins the
four diff colors explicitly so parsing never depends on user git config.
Blocks of fewer than 3 consecutive lines are below git's move-detection
threshold and will surface as residue — for a genuine tiny move, name it in the
commit message and the reviewer eyeballs those lines; that is the intended
manual surface, kept deliberately small.
"""

import re
import subprocess
import sys

# Pinned colors: 35=magenta (old moved), 36=cyan (new moved). git may emit
# them with attributes (e.g. \x1b[1;36m), so match the code anywhere in the
# leading escape sequence of the line.
MOVED_RE = re.compile(r"^\x1b\[(?:[0-9;]*;)?3[56](?:;[0-9;]*)?m")
ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Module-wiring lines a pure move is allowed to add/remove.
ALLOW = re.compile(
    r"^[+-]\s*("
    r"$"                                        # blank
    r"|(pub(\((crate|super)\))?\s+)?mod\s+\w+;" # mod / pub mod decl
    r"|(pub(\((crate|super)\))?\s+)?use\s"      # use / pub use
    r"|#\[path\s*=\s*\".*\"\]"                  # #[path] attr on a mod decl
    r"|//!"                                     # module-level doc comment
    r")"
)
HEADER = re.compile(r"^(\+\+\+|---)\s")


def main() -> int:
    args = [a for a in sys.argv[1:] if a != "--show-all"]
    show_all = "--show-all" in sys.argv
    if len(args) != 1:
        print(__doc__)
        return 2
    target = args[0]

    diff_args = [
        "git",
        "-c", "color.diff.oldMoved=magenta",
        "-c", "color.diff.newMoved=cyan",
        "-c", "color.diff.old=red",
        "-c", "color.diff.new=green",
        "diff",
        "--color=always",
        "--color-moved=plain",
        "--color-moved-ws=ignore-all-space",
    ]
    if target == "--cached":
        diff_args.append("--cached")
    elif ".." in target:
        diff_args.append(target)
    else:
        diff_args.append(f"{target}^!")

    out = subprocess.run(diff_args, capture_output=True, text=True, check=True).stdout

    residue: list[str] = []
    allowed = 0
    moved = 0
    for raw in out.splitlines():
        is_moved = bool(MOVED_RE.match(raw))
        plain = ANSI.sub("", raw)
        if not plain.startswith(("+", "-")) or HEADER.match(plain):
            continue
        if is_moved:
            moved += 1
            continue
        if ALLOW.match(plain):
            allowed += 1
            continue
        residue.append(plain)

    print(f"moved lines: {moved}  allowlisted wiring: {allowed}  residue: {len(residue)}")
    if residue:
        limit = len(residue) if show_all else 40
        for line in residue[:limit]:
            print(f"  RESIDUE {line}")
        if len(residue) > limit:
            print(f"  … {len(residue) - limit} more (--show-all to print)")
        print("NOT a pure move. Residue lines are semantic changes or sub-threshold")
        print("(<3-line) moves — split the commit or justify each line in review.")
        return 1
    print("PURE MOVE PROVEN: every non-wiring changed line is a detected move.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
