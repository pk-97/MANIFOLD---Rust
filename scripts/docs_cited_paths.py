#!/usr/bin/env python3
"""Report repo paths that the live docs cite but that no longer exist.

A nightly report (scripts/trunk_health.py), never a gate: a dead citation
misleads a reader but breaks nothing on stage. Live docs are `CLAUDE.md` and
`docs/**/*.md` outside the frozen records `docs/landings/` and
`docs/BUG_BACKLOG.md`. A citation is a `scripts/`, `.claude/`, `docs/` or
`crates/` token that is a file (last component has an extension) or a
directory written with a trailing `/`. Globs and placeholders are skipped: a
token followed by `*`, `<`, `{`, `$`, `[` or `…`, one ending in `-` or `_`,
and one whose file stem is a single character (`scripts/x.py`). Gitignored
paths (runtime state such as `.claude/worktrees/`) count as existing.

Fix a listed citation by pointing the doc at the current path or deleting it.
Always exits 0.
"""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PREFIXES = ("scripts/", ".claude/", "docs/", "crates/")
FROZEN = {Path("landings"), Path("BUG_BACKLOG.md")}


def is_path_char(c):
    return c.isascii() and (c.isalnum() or c in "_./-")


def live_docs(root):
    docs = root / "docs"
    out = [root / "CLAUDE.md"]
    stack = [docs]
    while stack:
        for path in sorted(stack.pop().iterdir()):
            if path.relative_to(docs) in FROZEN:
                continue
            if path.is_dir():
                stack.append(path)
            elif path.suffix == ".md":
                out.append(path)
    return sorted(out)


def cited_paths(text):
    """Repo paths cited in `text` that the report checks (see module doc)."""
    out, i, n = [], 0, len(text)
    while i < n:
        boundary = i == 0 or not (is_path_char(text[i - 1]) or text[i - 1] == "~")
        if not boundary or not is_path_char(text[i]):
            i += 1
            continue
        start = i
        while i < n and is_path_char(text[i]):
            i += 1
        token = text[start:i]
        if not token.startswith(PREFIXES):
            continue
        if i < n and text[i] in "*<{$[…":
            continue
        token = token.rstrip(".,:;")
        if token.endswith(("-", "_")) or ".." in token:
            continue
        last = token.rsplit("/", 1)[-1]
        stem, dot, ext = last.rpartition(".")
        is_file = bool(dot and stem and ext)
        if not (token.endswith("/") or is_file):
            continue
        if is_file and len(stem) <= 1:
            continue
        out.append(token)
    return out


def gitignored(root, paths):
    if not paths:
        return set()
    try:
        result = subprocess.run(["git", "-C", str(root), "check-ignore", "--stdin"],
                                input="".join(f"{p}\n" for p in paths),
                                capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return set()
    return set(result.stdout.splitlines())


def dead_citations(root):
    dead = set()
    for doc in live_docs(root):
        text = doc.read_text(errors="replace") if doc.is_file() else ""
        rel = doc.relative_to(root).as_posix()
        dead.update((rel, path) for path in cited_paths(text) if not (root / path).exists())
    ignored = gitignored(root, sorted({p for _, p in dead}))
    return sorted((d, p) for d, p in dead if p not in ignored and p.rstrip("/") not in ignored)


def main():
    dead = dead_citations(ROOT)
    for doc, path in dead:
        print(f"{doc}: {path}")
    print(f"docs-cited-paths: {len(dead)} dead citation(s) in live docs (report only)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
