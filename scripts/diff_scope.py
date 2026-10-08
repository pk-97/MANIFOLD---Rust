#!/usr/bin/env python3
"""Exclude docs and comment-only files using both sides of git diff -U0."""

import io
import re
import subprocess
import tokenize
from pathlib import Path

RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
CHAR = re.compile(r"'(?:\\[^\n]|[^'\\\n])'")

# Plain spans cannot change lexer state; jump to the next recognized token.
SPECIAL = re.compile(RAW_STRING.pattern + "|" + CHAR.pattern + r'|//|/\*|"')

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
                if quote:
                    out.append(text[i])
                    i += 1
                else:
                    special = SPECIAL.search(text, i + 1)
                    end = special.start() if special else len(text)
                    out.append(text[i:end])
                    i = end
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


def _parse_raw_patch(output):
    """Split one ``git diff --raw -z --patch`` result into changes and patches."""
    if not output:
        return [], []
    changes = []
    cursor = 0
    while cursor < len(output) and output[cursor] == ":":
        metadata_end = output.find("\0", cursor)
        if metadata_end < 0:
            raise RuntimeError("git diff returned a raw record without a path")
        path_end = output.find("\0", metadata_end + 1)
        if path_end < 0:
            raise RuntimeError("git diff returned a raw record without a terminator")
        fields = output[cursor + 1:metadata_end].split()
        if len(fields) < 5:
            raise RuntimeError("git diff returned a malformed raw record")
        changes.append({
            "old_mode": fields[0],
            "new_mode": fields[1],
            "old_oid": fields[2],
            "new_oid": fields[3],
            "status": fields[4],
            "path": output[metadata_end + 1:path_end],
        })
        cursor = path_end + 1
    if not changes or (cursor < len(output) and output[cursor] != "\0"):
        raise RuntimeError("git diff returned malformed raw output")
    if changes and cursor < len(output) and output[cursor] == "\0":
        cursor += 1
    patch_text = output[cursor:]
    patches = [match.group(0) for match in re.finditer(
        r"(?ms)^diff --git .*?(?=^diff --git |\Z)", patch_text)]
    if len(patches) != len(changes):
        raise RuntimeError("git diff raw records and patches do not match")
    return changes, patches


def _cat_file_batch(repo, object_ids):
    """Read all requested blobs with one ``git cat-file --batch`` process."""
    object_ids = list(dict.fromkeys(object_ids))
    if not object_ids:
        return {}
    request = "".join(f"{object_id}\n" for object_id in object_ids).encode()
    result = subprocess.run(
        ["git", "-C", str(repo), "cat-file", "--batch"],
        input=request,
        capture_output=True,
    )
    if result.returncode:
        error = result.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"git cat-file --batch failed: {error}")
    blobs = {}
    cursor = 0
    for object_id in object_ids:
        header_end = result.stdout.find(b"\n", cursor)
        if header_end < 0:
            raise RuntimeError("git cat-file --batch returned an incomplete header")
        header = result.stdout[cursor:header_end].split()
        if len(header) < 2 or header[1] == b"missing":
            raise RuntimeError(f"git cat-file --batch could not read {object_id}")
        if len(header) < 3:
            raise RuntimeError("git cat-file --batch returned a malformed header")
        size = int(header[2])
        data_start = header_end + 1
        data_end = data_start + size
        if data_end >= len(result.stdout) or result.stdout[data_end:data_end + 1] != b"\n":
            raise RuntimeError("git cat-file --batch returned truncated data")
        blobs[object_id] = result.stdout[data_start:data_end].decode()
        cursor = data_end + 1
    return blobs


def effective_paths(repo, base, head="HEAD"):
    """Code-bearing paths and ignored paths; head=None compares the working tree."""
    refs = [base, head] if head else [base]
    raw_patch = git(repo, "diff", "--raw", "-z", "--abbrev=40", "--patch", "--no-ext-diff", "--no-textconv",
                    "--no-renames", "-U0", *refs)
    changes, patches = _parse_raw_patch(raw_patch)
    object_ids = []
    for change in changes:
        suffix = Path(change["path"]).suffix
        if suffix in {".rs", ".wgsl", ".py"}:
            if not set(change["old_oid"]) == {"0"}:
                object_ids.append(change["old_oid"])
            if head and not set(change["new_oid"]) == {"0"}:
                object_ids.append(change["new_oid"])
    blobs = _cat_file_batch(repo, object_ids)
    active, ignored = [], []
    for index, change in enumerate(changes):
        path = change["path"]
        suffix = Path(path).suffix
        if suffix in {".md", ".txt"}:
            ignored.append(path)
            continue
        if suffix not in {".rs", ".wgsl", ".py"}:
            active.append(path)
            continue
        patch = patches[index] if index < len(patches) else ""
        old_oid, new_oid = change["old_oid"], change["new_oid"]
        before = "" if set(old_oid) == {"0"} else blobs[old_oid]
        if head:
            after = "" if set(new_oid) == {"0"} else blobs[new_oid]
        else:
            after = ("" if change["status"].startswith("D") else
                     (Path(repo) / path).read_text())
        mode_changed = (change["old_mode"] != change["new_mode"] and
                        change["old_mode"] != "000000" and change["new_mode"] != "000000")
        if not mode_changed and comment_only(patch, before, after, suffix):
            ignored.append(path)
        else:
            active.append(path)
    return active, ignored
