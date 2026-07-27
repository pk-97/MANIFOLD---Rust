#!/usr/bin/env python3
"""fix_bare_ids — mechanical fixer for the IDs-carry-names rule.

Companion to `.claude/hooks/bare-id-guard.py` (the gate; its docstring is the
spec). The guard blocks NEW bare IDs; this script retires the backlog by pure
joins — no model, no judgment:

  * bare bead ID  -> `BUG-xxxx (short title)`, title from `bd list --all`;
    old numeric BUG-NNN ids resolve through each bead's external_ref/title.
  * bare cross-doc §ref -> `FILE.md §N (heading title)`, title read from the
    target file's own heading (`## N Title` or `## §N Title`).

First prose mention per file gets the name (matching the guard's contract);
later mentions stay bare. Unresolvable IDs/refs are reported, never guessed.

  fix_bare_ids.py [--write] [paths...]     default: dry run over docs/*.md

Verify after a --write run: `.claude/hooks/bare-id-guard.py --audit` count
must go DOWN and `rg` a sample; the guard itself gates any regressions.
"""
import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GUARD = REPO / ".claude/hooks/bare-id-guard.py"

spec = importlib.util.spec_from_file_location("guard", GUARD)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

NUMERIC_RE = re.compile(r"\bBUG-\d{3}\b")
HEADING_RE = re.compile(r"^#{1,4}\s*§?([\w.]+)[.:\s—-]\s*(.+)$")


def short(title, limit=48):
    """Bead titles sometimes lead with the old numeric id — strip it."""
    title = re.sub(r"^BUG-\d+\s*[—:-]\s*", "", title).strip()
    if len(title) > limit:
        title = title[:limit].rsplit(" ", 1)[0].rstrip(",;:") + "…"
    return title


def bead_titles():
    out = subprocess.run(["bd", "list", "--all", "--json"],
                         capture_output=True, text=True, check=True).stdout
    titles = {}
    for b in json.loads(out):
        t = short(b.get("title", ""))
        if not t:
            continue
        titles[b["id"]] = t
        # old numeric ids: in external_ref and/or leading the title
        for src in (b.get("external_ref") or "", b.get("title") or ""):
            for num in NUMERIC_RE.findall(src):
                titles.setdefault(num, t)
    # pre-beads numeric bugs: BUG_BACKLOG.md (+ closed archive) carry
    # `BUG-NNN (name)` on their entry lines — the same join, older ledger.
    for ledger in (REPO / "docs/BUG_BACKLOG.md",
                   REPO / "docs/archive/BUG_BACKLOG_CLOSED.md"):
        if not ledger.exists():
            continue
        for num, name in re.findall(r"(BUG-\d{3}) \(([^)]+)\)", ledger.read_text()):
            titles.setdefault(num, short(name))
    return titles


def heading_title(target: Path, sec: str):
    if not target.exists():
        return None
    for line in target.read_text(errors="replace").splitlines():
        h = HEADING_RE.match(line)
        if h and h.group(1).rstrip(".") == sec.rstrip("."):
            return h.group(2).strip().rstrip(".")
    return None


def fix_file(path: Path, titles, write):
    text = path.read_text()
    lines = text.splitlines(keepends=True)
    named = guard.named_ids(text)
    fixed, unresolved = 0, []

    # Build the set of prose line indices (fences/commands exempt) once.
    prose = set()
    plain = [ln.rstrip("\n") for ln in lines]
    it = iter(enumerate(plain))
    in_fence = False
    for i, line in it:
        if guard.FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence or line.lstrip().startswith("$"):
            continue
        if guard.BD_CMD_RE.search(line) or "external_ref" in line:
            continue
        prose.add(i)

    done_ids = set(named)
    for i in sorted(prose):
        line = plain[i]
        out = line
        for mm in list(guard.ID_RE.finditer(line))[::-1]:
            bid = mm.group()
            if bid in done_ids:
                continue
            title = titles.get(bid)
            if not title:
                unresolved.append(f"{path.name}:{i+1} {bid} (no bead title)")
                done_ids.add(bid)
                continue
            end = mm.end()
            if end < len(out) and out[end] == "`":
                end += 1  # name goes outside a closing backtick
            out = out[:end] + f" ({title})" + out[end:]
            done_ids.add(bid)
            fixed += 1
        for sm in list(guard.SECREF_RE.finditer(line))[::-1]:
            pre = out[: sm.start()]
            fm = re.search(r"([\w./-]+\.md)[^§]*$", pre)
            if not fm or guard.NAME_PAREN_RE.match(out, sm.end()):
                continue
            target = fm.group(1)
            tpath = (path.parent / Path(target).name)
            if not tpath.exists():
                tpath = REPO / target.removeprefix("./")
            title = heading_title(tpath, sm.group()[1:])
            if not title:
                unresolved.append(f"{path.name}:{i+1} {target} {sm.group()} (no heading match)")
                continue
            out = out[: sm.end()] + f" ({title})" + out[sm.end():]
            fixed += 1
        if out != line:
            lines[i] = out + ("\n" if lines[i].endswith("\n") else "")

    if fixed and write:
        path.write_text("".join(lines))
    return fixed, unresolved


def main():
    write = "--write" in sys.argv
    paths = [Path(p) for p in sys.argv[1:] if not p.startswith("--")]
    if not paths:
        paths = sorted((REPO / "docs").glob("*.md"))
    titles = bead_titles()
    total, unresolved = 0, []
    for p in paths:
        n, u = fix_file(p, titles, write)
        total += n
        unresolved += u
        if n:
            print(f"{'fixed' if write else 'would fix'} {n:4}  {p.name}")
    print(f"\n{'fixed' if write else 'would fix'} {total} names; {len(unresolved)} unresolved")
    for u in unresolved[:40]:
        print(f"  UNRESOLVED {u}")
    if len(unresolved) > 40:
        print(f"  … and {len(unresolved) - 40} more")


if __name__ == "__main__":
    main()
