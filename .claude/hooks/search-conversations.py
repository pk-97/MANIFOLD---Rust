#!/usr/bin/env python3
"""
MANIFOLD Conversation Recall

Returns recent conversation context so Claude always knows what's been
happening across sessions. Factual output — Claude decides relevance.

Output:
  1. Rolling summary (last 7 days)
  2. Last N sessions with topic, commits, duration, areas
  3. If a prompt is provided, surfaces sessions with overlapping areas/keywords

No LLM calls. Pure text extraction from digest frontmatter.
"""

import re
import sys
from collections import Counter
from datetime import datetime, timedelta
from pathlib import Path

DIGEST_DIR = (
    Path.home()
    / ".claude/projects/-Users-peterkiemann-MANIFOLD---Rust/digests"
)

RECENT_COUNT = 6
ROLLING_DAYS = 7


def parse_digest_frontmatter(path):
    """Parse frontmatter fields from a digest file."""
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
            key = key.strip()
            val = val.strip()
            if key == "areas":
                val = [a.strip() for a in val.strip("[]").split(",") if a.strip()]
            meta[key] = val

    return meta


def rolling_summary(digests, days=ROLLING_DAYS):
    """One-line summary of the last N days of work."""
    cutoff = (datetime.now() - timedelta(days=days)).strftime("%Y-%m-%d")
    recent = [d for d in digests if (d.get("date") or "") >= cutoff]

    if not recent:
        return None

    area_counts = Counter()
    total_commits = 0
    pushed_count = 0
    for d in recent:
        areas = d.get("areas", [])
        if isinstance(areas, list):
            area_counts.update(areas)
        commits = d.get("commits", "0")
        try:
            total_commits += int(commits)
        except (ValueError, TypeError):
            pass
        if d.get("pushed") == "yes":
            pushed_count += 1

    top_areas = [a for a, _ in area_counts.most_common(5)]
    area_str = ", ".join(top_areas) if top_areas else "general"

    return (
        f"Last {days} days: {len(recent)} sessions across [{area_str}]. "
        f"{total_commits} commits, {pushed_count} pushed."
    )


def extract_prompt_keywords(text):
    """Extract area/topic keywords from the user's prompt for matching."""
    if not text:
        return set()

    text_lower = text.lower()
    keywords = set()

    # Match crate/module names
    crate_names = [
        "core", "editing", "playback", "gpu", "renderer", "media",
        "ui", "io", "native", "profiler", "led", "app", "audio",
    ]
    module_names = [
        "effects", "generators", "compositor", "sync", "transport",
        "export", "display-link", "vsync", "texture-pool", "ui-bridge",
    ]
    for name in crate_names + module_names:
        if name in text_lower:
            keywords.add(name)

    # Match compound terms
    compounds = {
        "fluid sim": "generators",
        "display link": "display-link",
        "frame pacing": "vsync",
        "scroll": "ui",
        "inspector": "ui",
        "timeline": "ui",
        "effect": "effects",
        "generator": "generators",
        "shader": "renderer",
        "wgsl": "renderer",
        "metal": "gpu",
        "undo": "editing",
        "midi": "playback",
        "osc": "playback",
        "ableton": "playback",
        "export": "media",
        "video": "media",
    }
    for phrase, area in compounds.items():
        if phrase in text_lower:
            keywords.add(area)

    return keywords


def format_session(meta):
    """Format one session line for output."""
    date = meta.get("date", "?")
    topic = meta.get("topic", "untitled")
    areas = meta.get("areas", [])
    commits = meta.get("commits", "0")
    pushed = meta.get("pushed", "no")
    duration = meta.get("duration", "?")
    sentiment = meta.get("sentiment", "neutral")
    session = meta.get("session_id", "")

    # Work indicator
    if commits != "0":
        if pushed == "yes":
            work = f"{commits}c pushed"
        else:
            work = f"{commits}c"
    else:
        work = "no commits"

    # Duration — just the label part
    dur_label = duration.split("(")[0].strip() if duration else "?"

    # Sentiment — only show if notable
    sent_str = ""
    if sentiment in ("frustrating", "frustrating-then-resolved"):
        sent_str = " | rough"
    elif sentiment == "smooth":
        sent_str = " | smooth"

    area_str = f" [{', '.join(areas)}]" if isinstance(areas, list) and areas else ""

    return (
        f"- [{date}] {topic} — {work}, {dur_label}{sent_str}{area_str}\n"
        f"  digest: digests/{session}.md"
    )


def main():
    if not DIGEST_DIR.exists():
        sys.exit(0)

    # Get prompt from args (for continuation detection)
    prompt = " ".join(sys.argv[1:]) if len(sys.argv) > 1 else ""

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

    # Sort by date descending
    digests.sort(key=lambda m: m.get("date", ""), reverse=True)

    # Build output
    lines = ["<prior-work>"]

    # Rolling summary
    summary = rolling_summary(digests)
    if summary:
        lines.append(summary)
        lines.append("")

    # Continuation detection — if prompt overlaps with recent session areas
    if prompt:
        keywords = extract_prompt_keywords(prompt)
        if keywords:
            # Find recent sessions (last 14 days) with overlapping areas
            cutoff = (datetime.now() - timedelta(days=14)).strftime("%Y-%m-%d")
            relevant = []
            for d in digests:
                if (d.get("date") or "") < cutoff:
                    break
                d_areas = set(d.get("areas", []) if isinstance(d.get("areas"), list) else [])
                overlap = keywords & d_areas
                if overlap:
                    relevant.append(d)

            # Only show if we found something not already in the recent list
            recent_ids = {d["session_id"] for d in digests[:RECENT_COUNT]}
            extra = [d for d in relevant[:3] if d["session_id"] not in recent_ids]
            if extra:
                lines.append("Related prior sessions:")
                for meta in extra:
                    lines.append(format_session(meta))
                lines.append("")

    # Recent sessions
    recent = digests[:RECENT_COUNT]
    lines.append(f"Last {len(recent)} sessions:")
    for meta in recent:
        lines.append(format_session(meta))

    lines.append("</prior-work>")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
