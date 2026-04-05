#!/usr/bin/env python3
"""
MANIFOLD Conversation Recall

Returns the most recent conversation digests so Claude always has context
on what was discussed in prior sessions. Claude decides what's relevant,
not this script.

Usage:
    python3 search-conversations.py "user message text"
    echo "user message text" | python3 search-conversations.py
"""

import sys
from pathlib import Path

DIGEST_DIR = (
    Path.home()
    / ".claude/projects/-Users-peterkiemann-MANIFOLD---Rust/digests"
)

MAX_RESULTS = 8


def parse_digest_frontmatter(path):
    """Parse frontmatter from a digest file."""
    meta = {"path": path, "session_id": path.stem}
    in_frontmatter = False
    past_frontmatter = False

    try:
        text = path.read_text()
    except Exception:
        return None

    for line in text.split("\n"):
        if line.strip() == "---":
            if not in_frontmatter and not past_frontmatter:
                in_frontmatter = True
                continue
            elif in_frontmatter:
                in_frontmatter = False
                past_frontmatter = True
                continue
        if in_frontmatter and ":" in line:
            key, _, val = line.partition(":")
            meta[key.strip()] = val.strip()

    return meta


def format_result(meta):
    """Format a single result as a concise pointer line."""
    date = meta.get("date", "?")
    topic = meta.get("topic", "untitled")
    status = meta.get("status", "?")
    sentiment = meta.get("sentiment", "neutral")
    areas = meta.get("areas", "").strip("[]")
    session = meta.get("session_id", "")

    if status == "resolved":
        status_str = "resolved"
    elif status == "abandoned":
        status_str = "dropped"
    else:
        status_str = "OPEN"

    sentiment_str = ""
    if sentiment in ("frustrating", "frustrating-then-resolved"):
        sentiment_str = " | rough session"
    elif sentiment == "smooth":
        sentiment_str = " | smooth"

    areas_str = f" [{areas}]" if areas else ""

    return (
        f"- [{date}] {topic} — {status_str}{sentiment_str}{areas_str}\n"
        f"  digest: digests/{session}.md"
    )


def main():
    if not DIGEST_DIR.exists():
        sys.exit(0)

    # Load all digests
    digests = []
    for p in DIGEST_DIR.glob("*.md"):
        if p.name == "INDEX.md":
            continue
        meta = parse_digest_frontmatter(p)
        if meta:
            digests.append(meta)

    if not digests:
        sys.exit(0)

    # Sort by date descending, most recent first
    digests.sort(key=lambda m: m.get("date", ""), reverse=True)

    top = digests[:MAX_RESULTS]

    print("<prior-work>")
    print(f"Last {len(top)} sessions (read digest file for full context):")
    for meta in top:
        print(format_result(meta))
    print("</prior-work>")


if __name__ == "__main__":
    main()
