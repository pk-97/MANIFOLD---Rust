#!/usr/bin/env python3
"""Exclude docs and comment-only files using both sides of git diff -U0."""

import io
import re
import subprocess
import tokenize
from pathlib import Path

RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHAR = re.compile(r"'(?:\\[^\n]|[^'\\\n])'")


def code_lines(text, suffix):
    """Non-comment text per line; strings remain code, including multiline ones."""
    if suffix == ".py":
        lines = text.splitlines(keepends=True)
        try:
            string_rows = set()
            for token in tokenize.generate_tokens(io.StringIO(text).readline):
                if token.type == tokenize.COMMENT:
                    row, col = token.start
                    lines[row - 1] = lines[row - 1][:col] + "\n"
                elif token.type == tokenize.STRING:
                    string_rows.update(range(token.start[0], token.end[0] + 1))
            for row in string_rows:
                lines[row - 1] = "STRING " + lines[row - 1]
        except (tokenize.TokenError, IndentationError):
            return text.splitlines()  # Invalid source must still be checked.
        return "".join(lines).splitlines()
    out, i, depth, quote, raw = [], 0, 0, False, None
    while i < len(text):
        if depth:
            if text.startswith("/*", i):
                depth += 1
                i += 2
            elif text.startswith("*/", i):
                depth -= 1
                i += 2
            else:
                out.append("\n" if text[i] == "\n" else " ")
                i += 1
        elif raw is not None:
            if text.startswith(raw, i):
                out.append(raw)
                i += len(raw)
                raw = None
            else:
                out.append("STRING\n" if text[i] == "\n" else text[i])
                i += 1
        elif quote:
            out.append("STRING\n" if text[i] == "\n" else text[i])
            if text[i] == "\\" and i + 1 < len(text):
                out.append(text[i + 1])
                i += 2
                continue
            if text[i] == '"':
                quote = False
            i += 1
        elif text.startswith("//", i):
            end = text.find("\n", i)
            i = len(text) if end < 0 else end
        elif text.startswith("/*", i):
            out.append(" ")
            depth = 1
            i += 2
        else:
            match = RAW_STRING.match(text, i)
            char = CHAR.match(text, i)
            if match:
                out.append(match[0])
                raw = '"' + match[1]
                i += len(match[0])
            elif char:
                out.append(char[0])
                i += len(char[0])
            else:
                quote = text[i] == '"'
                out.append(text[i])
                i += 1
    return "".join(out).splitlines()


def comment_only(patch, before, after, suffix):
    if suffix not in {".rs", ".wgsl", ".py"}:
        return False
    old, new = code_lines(before, suffix), code_lines(after, suffix)
    # Moving a comment delimiter can expose/hide unchanged lines of code.
    if [s for s in old if s.strip()] != [s for s in new if s.strip()]:
        return False
    changed = False
    for line in patch.splitlines():
        match = re.match(r"@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", line)
        if not match:
            continue
        changed = True
        for lines, start, count in ((old, match[1], match[2]), (new, match[3], match[4])):
            start, count = int(start), int(count) if count is not None else 1
            if any(s.strip() for s in lines[start - 1:start - 1 + count]):
                return False
    return changed


def git(repo, *args):
    result = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout


def effective_paths(repo, base, head="HEAD"):
    """Code-bearing paths and ignored paths; head=None compares the working tree."""
    refs = [base, head] if head else [base]
    paths = git(repo, "diff", "--name-only", "--no-renames", "-z", *refs).split("\0")
    active, ignored = [], []
    for path in filter(None, paths):
        suffix = Path(path).suffix
        if suffix in {".md", ".txt"}:
            ignored.append(path)
            continue
        if suffix not in {".rs", ".wgsl", ".py"}:
            active.append(path)
            continue
        patch = git(repo, "diff", "--no-ext-diff", "--no-textconv", "--no-renames", "-U0", *refs, "--", path)
        # Additions/deletions use an empty side; mode changes remain active.
        before = "" if "new file mode" in patch else git(repo, "show", f"{base}:{path}")
        after = ("" if "deleted file mode" in patch else
                 git(repo, "show", f"{head}:{path}") if head else (Path(repo) / path).read_text())
        if ("old mode" not in patch and comment_only(patch, before, after, suffix)):
            ignored.append(path)
        else:
            active.append(path)
    return active, ignored
