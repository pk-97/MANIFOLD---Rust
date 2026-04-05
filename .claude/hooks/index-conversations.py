#!/usr/bin/env python3
"""
MANIFOLD Conversation Indexer

Processes raw Claude Code .jsonl conversation files into structured digests.
Pure text extraction — no LLM calls, no embeddings.

Signals extracted:
  - Topic (from ai-title)
  - User messages (the actual conversation thread)
  - Files touched (from Edit/Write tool calls)
  - Commits (from git commit commands)
  - Areas (derived from file paths)
  - Sentiment (keyword analysis on user messages)
  - Status (resolved/open/abandoned)
"""

import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

PROJECT_DIR = Path.home() / ".claude/projects/-Users-peterkiemann-MANIFOLD---Rust"
JSONL_DIR = PROJECT_DIR
DIGEST_DIR = PROJECT_DIR / "digests"
INDEX_FILE = DIGEST_DIR / "INDEX.md"

# Strip the home dir prefix from file paths for readability
HOME = str(Path.home())
PROJECT_ROOT = "MANIFOLD - Rust"

# --- Sentiment keywords ---

CORRECTION_WORDS = [
    "no ", "no,", "not that", "wrong", "stop", "I said", "I told you",
    "that's not", "thats not", "don't ", "didn't ask", "not what I",
    "why did you", "why are you", "undo that", "the exact opposite",
    "that's incorrect", "not what I asked",
]
FRUSTRATION_WORDS = [
    "CRITICAL", "worse", "broken", "locks up", "lock up", "nothing works",
    "not working", "still broken", "doesn't work", "failed", "crashes",
    "crash", "panic", "everything is", "completely", "terrible", "unusable",
    "catastrophic", "heart breaking", "kills Manifold", "kinda kills",
    "HARD LOCK", "Are you serious", "Ugh",
]
AFFIRMATION_WORDS = [
    "perfect", "exactly", "nice", "cool", "good", "great", "yes please",
    "looks good", "that's it", "that was it", "works", "working",
    "nailed it", "love it", "sounds good", "yes!", "awesome",
    "very very nice", "let's go for it", "really nice",
]
ABANDONMENT_SIGNALS = [
    "never mind", "nevermind", "forget it", "let's move on", "skip this",
    "not now", "later", "stop",
]

# Crate name to short area tag
CRATE_AREAS = {
    "manifold-core": "core",
    "manifold-editing": "editing",
    "manifold-playback": "playback",
    "manifold-gpu": "gpu",
    "manifold-renderer": "renderer",
    "manifold-media": "media",
    "manifold-ui": "ui",
    "manifold-io": "io",
    "manifold-native": "native",
    "manifold-profiler": "profiler",
    "manifold-led": "led",
    "manifold-app": "app",
}

# Sub-module areas worth tagging
MODULE_AREAS = {
    "effects": "effects",
    "generators": "generators",
    "ui_bridge": "ui-bridge",
    "compositor": "compositor",
    "sync": "sync",
    "transport": "transport",
    "export": "export",
    "line_pipeline": "line-pipeline",
    "texture_pool": "texture-pool",
    "display_link": "display-link",
    "vsync": "vsync",
}


def extract_text_from_content(content):
    """Pull plain text from a message content field (string or list)."""
    if isinstance(content, str):
        return content.strip()
    if isinstance(content, list):
        parts = []
        for c in content:
            if isinstance(c, dict) and c.get("type") == "text":
                parts.append(c["text"].strip())
        return "\n".join(parts)
    return ""


def is_system_noise(text):
    """Filter out system-injected content that isn't the user talking."""
    if not text:
        return True
    stripped = text.strip()
    if stripped.startswith("<"):
        # Could be <system-reminder>, <ide_selection>, etc.
        # But some user messages legitimately start with < (rare)
        if any(stripped.startswith(tag) for tag in [
            "<system-reminder>", "<ide_selection>", "<ide_opened_file>",
            "<ide_viewport>", "<command-name>",
        ]):
            return True
    return False


def extract_user_text(text):
    """Extract real user text, stripping system tags that appear inline."""
    if not text:
        return ""
    # Remove system-reminder blocks
    cleaned = re.sub(r"<system-reminder>.*?</system-reminder>", "", text, flags=re.DOTALL)
    # Remove ide tags
    cleaned = re.sub(r"<ide_\w+>.*?</ide_\w+>", "", cleaned, flags=re.DOTALL)
    cleaned = re.sub(r"<ide_opened_file>.*?</ide_opened_file>", "", cleaned, flags=re.DOTALL)
    return cleaned.strip()


def parse_session(jsonl_path):
    """Parse a .jsonl file and extract all signals."""
    session_id = jsonl_path.stem
    title = None
    user_messages = []
    files_touched = set()
    commits = []
    timestamp_first = None
    timestamp_last = None
    exchange_count = 0
    interrupted_count = 0

    with open(jsonl_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue

            msg_type = obj.get("type", "")

            # Title
            if msg_type == "ai-title":
                title = obj.get("aiTitle", "")
                continue

            # Timestamps
            ts = obj.get("timestamp")
            if ts:
                if timestamp_first is None:
                    timestamp_first = ts
                timestamp_last = ts

            # User messages
            if msg_type == "user":
                msg = obj.get("message", {})
                raw_text = extract_text_from_content(msg.get("content", ""))

                # Check for interrupts
                if "[Request interrupted by user]" in raw_text:
                    interrupted_count += 1

                user_text = extract_user_text(raw_text)
                if user_text and not is_system_noise(user_text):
                    user_messages.append(user_text)
                exchange_count += 1

            # Assistant tool calls
            if msg_type == "assistant":
                msg = obj.get("message", {})
                content = msg.get("content", [])
                if isinstance(content, list):
                    for block in content:
                        if not isinstance(block, dict):
                            continue
                        if block.get("type") != "tool_use":
                            continue
                        tool_name = block.get("name", "")
                        inp = block.get("input", {})

                        # File edits
                        if tool_name in ("Edit", "Write"):
                            fp = inp.get("file_path", "")
                            if fp and PROJECT_ROOT in fp:
                                # Normalize to relative path
                                idx = fp.find(PROJECT_ROOT)
                                rel = fp[idx + len(PROJECT_ROOT) + 1:]
                                files_touched.add(rel)

                        # Git commits
                        if tool_name == "Bash":
                            cmd = inp.get("command", "")
                            if "git commit" in cmd:
                                # Heredoc style: $(cat <<'EOF'\nmessage\nEOF\n)
                                m = re.search(
                                    r"<<'?EOF'?\s*\n(.+?)\nEOF",
                                    cmd, re.DOTALL,
                                )
                                if m:
                                    first_line = (
                                        m.group(1).strip().split("\n")[0]
                                    )
                                    commits.append(first_line[:120])
                                else:
                                    # Simple -m "message" style
                                    m = re.search(
                                        r'-m\s+["\'](.+?)["\']', cmd
                                    )
                                    if m:
                                        commits.append(m.group(1)[:120])

    # Derive date from first timestamp
    date_str = None
    if timestamp_first:
        try:
            dt = datetime.fromisoformat(timestamp_first.replace("Z", "+00:00"))
            date_str = dt.strftime("%Y-%m-%d")
        except (ValueError, AttributeError):
            pass

    return {
        "session_id": session_id,
        "title": title,
        "date": date_str,
        "user_messages": user_messages,
        "files_touched": sorted(files_touched),
        "commits": commits,
        "exchange_count": exchange_count,
        "interrupted_count": interrupted_count,
        "timestamp_first": timestamp_first,
        "timestamp_last": timestamp_last,
    }


def derive_areas(files_touched):
    """Extract area tags from file paths."""
    areas = set()
    for fp in files_touched:
        # Crate-level
        for crate, tag in CRATE_AREAS.items():
            if crate in fp:
                areas.add(tag)
                break
        # Module-level
        for module, tag in MODULE_AREAS.items():
            if module in fp:
                areas.add(tag)
    return sorted(areas)


def score_sentiment(user_messages):
    """Score conversation sentiment from user messages."""
    all_text = " ".join(user_messages).lower()
    all_text_original = " ".join(user_messages)

    corrections = sum(1 for w in CORRECTION_WORDS if w.lower() in all_text)
    frustrations = sum(1 for w in FRUSTRATION_WORDS if w.lower() in all_text)
    affirmations = sum(1 for w in AFFIRMATION_WORDS if w.lower() in all_text)
    # Check for ALL CAPS words (frustration signal) — but not acronyms
    caps_words = len(re.findall(r"\b[A-Z]{4,}\b", all_text_original))
    # Discount common acronyms
    acronyms = len(re.findall(
        r"\b(?:MIDI|WGSL|GPU|CPU|FPS|HDR|SDR|VSync|ACES|LLM|API|OSC|UI|PR)\b",
        all_text_original
    ))
    caps_frustration = max(0, caps_words - acronyms)

    neg_score = corrections * 2 + frustrations * 3 + caps_frustration
    pos_score = affirmations * 2

    if neg_score == 0 and pos_score == 0:
        return "neutral", "standard session"

    if neg_score > 6 and pos_score > 4:
        return "frustrating-then-resolved", "rough start but got there"
    if neg_score > 6:
        return "frustrating", "multiple corrections or failures"
    if neg_score > 2 and pos_score > neg_score:
        return "mixed-productive", "some friction but overall positive"
    if pos_score > 4:
        return "smooth", "clean progression"
    if pos_score > 0:
        return "productive", "generally positive"

    return "neutral", "standard session"


def determine_status(parsed, sentiment_label):
    """Determine session status: resolved, open, abandoned, exploratory."""
    has_commits = len(parsed["commits"]) > 0
    was_interrupted = parsed["interrupted_count"] > 0
    msg_count = len(parsed["user_messages"])

    if has_commits:
        return "resolved"
    if sentiment_label == "frustrating" and not has_commits:
        return "open"
    if was_interrupted and msg_count <= 3:
        return "abandoned"
    if msg_count <= 2:
        return "brief"
    return "open"


def extract_open_items(user_messages, commits):
    """Try to identify unresolved items from trailing messages."""
    if not user_messages:
        return []

    open_markers = [
        "TODO", "todo", "still need", "remaining", "later", "next time",
        "pending", "doesn't work yet", "not fixed", "investigate",
        "come back to", "needs more",
    ]

    items = []
    # Check last 3 user messages for open markers
    for msg in user_messages[-3:]:
        for marker in open_markers:
            if marker.lower() in msg.lower():
                # Truncate to a useful snippet
                snippet = msg[:150].strip()
                if snippet not in items:
                    items.append(snippet)
                break

    return items


def build_digest(parsed):
    """Build a structured markdown digest from parsed data."""
    areas = derive_areas(parsed["files_touched"])
    sentiment_label, sentiment_note = score_sentiment(parsed["user_messages"])
    status = determine_status(parsed, sentiment_label)
    open_items = extract_open_items(parsed["user_messages"], parsed["commits"])

    # Build the thread summary — just user messages, compressed
    thread_lines = []
    for msg in parsed["user_messages"]:
        compressed = msg[:200].replace("\n", " ").strip()
        if compressed:
            thread_lines.append(f"- {compressed}")

    lines = [
        "---",
        f"session: {parsed['session_id']}",
        f"date: {parsed['date'] or 'unknown'}",
        f"topic: {parsed['title'] or 'untitled session'}",
        f"areas: [{', '.join(areas)}]" if areas else "areas: []",
        f"files_touched: {len(parsed['files_touched'])}",
        f"status: {status}",
        f"sentiment: {sentiment_label}",
        f"exchanges: {parsed['exchange_count']}",
        "---",
        "",
    ]

    # Sentiment note if notable
    if sentiment_label not in ("neutral", "productive"):
        lines.append(f"**Sentiment:** {sentiment_note}")
        lines.append("")

    # Commits
    if parsed["commits"]:
        lines.append("**Commits:**")
        for c in parsed["commits"]:
            lines.append(f"- {c}")
        lines.append("")

    # Open items
    if open_items:
        lines.append("**Open items:**")
        for item in open_items:
            lines.append(f"- {item}")
        lines.append("")

    # Files touched (compact)
    if parsed["files_touched"]:
        lines.append("**Files:**")
        for fp in parsed["files_touched"][:15]:  # Cap at 15
            lines.append(f"- {fp}")
        if len(parsed["files_touched"]) > 15:
            lines.append(f"- ... and {len(parsed['files_touched']) - 15} more")
        lines.append("")

    # Thread (user messages compressed)
    if thread_lines:
        lines.append("**Thread:**")
        lines.extend(thread_lines[:20])  # Cap at 20 messages
        if len(thread_lines) > 20:
            lines.append(f"- ... {len(thread_lines) - 20} more messages")
        lines.append("")

    return "\n".join(lines)


def build_index(digests):
    """Build the master INDEX.md from all digests, sorted by date descending."""
    # Sort by date descending, then session_id
    digests.sort(key=lambda d: d.get("date") or "0000-00-00", reverse=True)

    lines = [
        "# Conversation Digest Index",
        "",
        "Auto-generated by index-conversations.py. Do not edit manually.",
        "",
        "Format: `[date] topic (status, sentiment) — areas`",
        "",
    ]

    current_month = None
    for d in digests:
        date = d.get("date", "unknown")
        month = date[:7] if date and date != "unknown" else "unknown"

        if month != current_month:
            current_month = month
            lines.append(f"## {month}")
            lines.append("")

        topic = d.get("topic", "untitled")
        status = d.get("status", "?")
        sentiment = d.get("sentiment", "neutral")
        areas = d.get("areas", [])
        session = d.get("session_id", "")

        # Status + sentiment indicator
        if status == "resolved":
            indicator = "done"
        elif status == "abandoned":
            indicator = "dropped"
        else:
            indicator = "open"

        if sentiment in ("frustrating", "frustrating-then-resolved"):
            indicator += ", rough"
        elif sentiment == "smooth":
            indicator += ", smooth"

        area_str = ", ".join(areas[:4]) if areas else "general"
        lines.append(
            f"- [{date}]({session}.md) {topic} ({indicator}) — {area_str}"
        )

    lines.append("")
    return "\n".join(lines)


def main():
    force = "--force" in sys.argv
    DIGEST_DIR.mkdir(parents=True, exist_ok=True)

    # Find all .jsonl files
    jsonl_files = sorted(JSONL_DIR.glob("*.jsonl"))

    # Check which are already indexed
    existing = set()
    if not force:
        existing = {p.stem for p in DIGEST_DIR.glob("*.md") if p.name != "INDEX.md"}

    to_process = []
    for jf in jsonl_files:
        if jf.stem not in existing or force:
            to_process.append(jf)

    print(f"Found {len(jsonl_files)} conversations, {len(to_process)} new to index")

    all_digests_meta = []

    # Process new files
    for jf in to_process:
        try:
            parsed = parse_session(jf)
        except Exception as e:
            print(f"  SKIP {jf.stem}: {e}")
            continue

        # Skip very short/empty sessions
        if not parsed["user_messages"] and not parsed["title"]:
            continue

        digest_text = build_digest(parsed)
        digest_path = DIGEST_DIR / f"{jf.stem}.md"
        digest_path.write_text(digest_text)

    # Rebuild index from ALL digests
    for dp in sorted(DIGEST_DIR.glob("*.md")):
        if dp.name == "INDEX.md":
            continue
        try:
            text = dp.read_text()
            # Parse frontmatter
            meta = {"session_id": dp.stem}
            in_frontmatter = False
            for line in text.split("\n"):
                if line.strip() == "---":
                    if not in_frontmatter:
                        in_frontmatter = True
                        continue
                    else:
                        break
                if in_frontmatter and ":" in line:
                    key, _, val = line.partition(":")
                    key = key.strip()
                    val = val.strip()
                    if key == "areas":
                        val = [
                            a.strip()
                            for a in val.strip("[]").split(",")
                            if a.strip()
                        ]
                    meta[key] = val
            all_digests_meta.append(meta)
        except Exception:
            continue

    # Write index
    index_text = build_index(all_digests_meta)
    INDEX_FILE.write_text(index_text)

    print(f"Indexed {len(all_digests_meta)} conversations → {DIGEST_DIR}")


if __name__ == "__main__":
    main()
