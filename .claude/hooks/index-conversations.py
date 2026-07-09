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
  - Git pushes (strongest signal of completed work)
  - Areas (derived from file paths)
  - Sentiment (phrase analysis on direct user speech only)
  - Duration (from first/last timestamp)
"""

import json
import re
import sys
from datetime import datetime, timedelta
from pathlib import Path

PROJECT_DIR = Path.home() / ".claude/projects/-Users-peterkiemann-MANIFOLD---Rust"
JSONL_DIR = PROJECT_DIR
DIGEST_DIR = PROJECT_DIR / "digests"
INDEX_FILE = DIGEST_DIR / "INDEX.md"

PROJECT_ROOT = "MANIFOLD - Rust"

# Digest pruning is effectively OFF (2026-07-09, Peter's call): the full
# corpus — raw transcripts AND digests — is retained for offline daemon
# tuning and replay-testing agents against captured history. The built-in
# transcript cleanup is likewise disabled via cleanupPeriodDays in
# ~/.claude/settings.json. The searcher still only matches the last 14 days,
# so old digests cost disk, not context.
DIGEST_RETENTION_DAYS = 36500

# --- Sentiment keywords ---
#
# These detect the USER's attitude toward the COLLABORATION, not the
# state of the software. "The app crashes" is a bug report, not frustration.
# "You keep breaking it" is frustration.

# Direct corrections of Claude's behavior (high signal)
CORRECTION_PHRASES = [
    "I said", "I told you", "I already said", "I just said",
    "that's not what I asked", "not what I asked", "not what I meant",
    "that's incorrect", "that's wrong", "you're wrong",
    "why did you", "why are you", "why would you",
    "undo that", "revert that", "put it back", "the exact opposite",
    "didn't ask for", "don't do that", "stop doing",
    "read it again", "re-read", "look again",
    "I specifically said", "I explicitly",
]

# Genuine interpersonal frustration (not bug descriptions)
FRUSTRATION_PHRASES = [
    "are you serious", "seriously?",
    "this is getting nowhere", "going in circles",
    "you keep", "you already", "you just did",
    "that made it worse", "you broke", "you're breaking",
    "completely wrong", "totally wrong",
    "heart breaking", "catastrophic",
    "unusable", "terrible",
]

# Positive collaboration signals
AFFIRMATION_PHRASES = [
    "perfect", "exactly", "nailed it", "love it", "awesome",
    "that's it", "that was it", "that fixed it",
    "looks good", "works", "working now",
    "nice", "great", "yes please", "sounds good",
    "very nice", "let's go", "spot on",
    "much better", "way better",
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
    "manifold-audio": "audio",
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
    cleaned = re.sub(r"<system-reminder>.*?</system-reminder>", "", text, flags=re.DOTALL)
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
    has_push = False
    timestamp_first = None
    timestamp_last = None
    exchange_count = 0

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

            if msg_type == "ai-title":
                title = obj.get("aiTitle", "")
                continue

            ts = obj.get("timestamp")
            if ts:
                if timestamp_first is None:
                    timestamp_first = ts
                timestamp_last = ts

            if msg_type == "user":
                msg = obj.get("message", {})
                raw_text = extract_text_from_content(msg.get("content", ""))
                user_text = extract_user_text(raw_text)
                if user_text and not is_system_noise(user_text):
                    user_messages.append(user_text)
                exchange_count += 1

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

                        if tool_name in ("Edit", "Write"):
                            fp = inp.get("file_path", "")
                            if fp and PROJECT_ROOT in fp:
                                idx = fp.find(PROJECT_ROOT)
                                rel = fp[idx + len(PROJECT_ROOT) + 1:]
                                files_touched.add(rel)

                        if tool_name == "Bash":
                            cmd = inp.get("command", "")
                            if "git commit" in cmd:
                                m = re.search(
                                    r"<<'?EOF'?\s*\n(.+?)\nEOF",
                                    cmd, re.DOTALL,
                                )
                                if m:
                                    first_line = m.group(1).strip().split("\n")[0]
                                    commits.append(first_line[:120])
                                else:
                                    m = re.search(r'-m\s+["\'](.+?)["\']', cmd)
                                    if m:
                                        commits.append(m.group(1)[:120])
                            if "git push" in cmd:
                                has_push = True

    # Derive date and duration
    date_str = None
    duration_minutes = 0
    if timestamp_first:
        try:
            dt_first = datetime.fromisoformat(timestamp_first.replace("Z", "+00:00"))
            date_str = dt_first.strftime("%Y-%m-%d")
            if timestamp_last:
                dt_last = datetime.fromisoformat(timestamp_last.replace("Z", "+00:00"))
                duration_minutes = int((dt_last - dt_first).total_seconds() / 60)
        except (ValueError, AttributeError):
            pass

    return {
        "session_id": session_id,
        "title": title,
        "date": date_str,
        "user_messages": user_messages,
        "files_touched": sorted(files_touched),
        "commits": commits,
        "has_push": has_push,
        "exchange_count": exchange_count,
        "duration_minutes": duration_minutes,
        "timestamp_first": timestamp_first,
        "timestamp_last": timestamp_last,
    }


def derive_areas(files_touched):
    """Extract area tags from file paths."""
    areas = set()
    for fp in files_touched:
        for crate, tag in CRATE_AREAS.items():
            if crate in fp:
                areas.add(tag)
                break
        for module, tag in MODULE_AREAS.items():
            if module in fp:
                areas.add(tag)
    return sorted(areas)


def duration_label(minutes):
    """Human-readable duration bucket."""
    if minutes < 5:
        return "brief"
    if minutes < 30:
        return "short"
    if minutes < 90:
        return "medium"
    return "long"


def strip_technical_content(text):
    """Remove code, errors, and paths — only score direct speech."""
    text = re.sub(r"```.*?```", "", text, flags=re.DOTALL)
    text = re.sub(r"`[^`]+`", "", text)
    text = re.sub(r"[\w/\\.-]+\.(rs|wgsl|toml|json|md|py|sh)\b", "", text)
    text = re.sub(r"^.*(?:thread '|error\[|warning\[|panicked at|SIGABRT|EXC_BAD).*$",
                  "", text, flags=re.MULTILINE)
    text = re.sub(r"^.*(?:cargo |git |rm |ls |cat ).*$", "", text, flags=re.MULTILINE)
    return text


def score_sentiment(user_messages):
    """Score based on user's attitude toward the collaboration, not the software."""
    cleaned = strip_technical_content(" ".join(user_messages))
    text_lower = cleaned.lower()

    corrections = sum(1 for p in CORRECTION_PHRASES if p.lower() in text_lower)
    frustrations = sum(1 for p in FRUSTRATION_PHRASES if p.lower() in text_lower)
    affirmations = sum(1 for p in AFFIRMATION_PHRASES if p.lower() in text_lower)

    neg_score = corrections * 3 + frustrations * 2
    pos_score = affirmations * 2

    if neg_score == 0 and pos_score == 0:
        return "neutral"
    if neg_score >= 10 and pos_score >= 6:
        return "frustrating-then-resolved"
    if neg_score >= 10:
        return "frustrating"
    if neg_score >= 4 and pos_score > neg_score:
        return "mixed"
    if pos_score >= 6:
        return "smooth"
    if pos_score > 0:
        return "productive"
    return "neutral"


def build_digest(parsed):
    """Build a structured markdown digest from parsed data."""
    areas = derive_areas(parsed["files_touched"])
    sentiment = score_sentiment(parsed["user_messages"])
    dur = duration_label(parsed["duration_minutes"])

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
        f"commits: {len(parsed['commits'])}",
        f"pushed: {'yes' if parsed['has_push'] else 'no'}",
        f"duration: {dur} ({parsed['duration_minutes']}m)",
        f"sentiment: {sentiment}",
        f"exchanges: {parsed['exchange_count']}",
        "---",
        "",
    ]

    if parsed["commits"]:
        lines.append("**Commits:**")
        for c in parsed["commits"]:
            lines.append(f"- {c}")
        lines.append("")

    if parsed["files_touched"]:
        lines.append("**Files:**")
        for fp in parsed["files_touched"][:15]:
            lines.append(f"- {fp}")
        if len(parsed["files_touched"]) > 15:
            lines.append(f"- ... and {len(parsed['files_touched']) - 15} more")
        lines.append("")

    if thread_lines:
        lines.append("**Thread:**")
        lines.extend(thread_lines[:20])
        if len(thread_lines) > 20:
            lines.append(f"- ... {len(thread_lines) - 20} more messages")
        lines.append("")

    return "\n".join(lines)


def build_index(digests):
    """Build the master INDEX.md sorted by date descending."""
    digests.sort(key=lambda d: d.get("date") or "0000-00-00", reverse=True)

    lines = [
        "# Conversation Digest Index",
        "",
        "Auto-generated. Do not edit manually.",
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
        areas = d.get("areas", [])
        session = d.get("session_id", "")
        commits = d.get("commits", "0")
        pushed = d.get("pushed", "no")
        duration = d.get("duration", "")

        work_str = f"{commits}c" if commits != "0" else "no commits"
        if pushed == "yes":
            work_str += ", pushed"

        area_str = ", ".join(areas[:4]) if areas else "general"
        lines.append(
            f"- [{date}]({session}.md) {topic} ({work_str}, {duration}) — {area_str}"
        )

    lines.append("")
    return "\n".join(lines)


def cleanup_old_digests(days_to_keep=DIGEST_RETENTION_DAYS):
    """Prune digests whose frontmatter `date:` field is older than days_to_keep.

    Uses the conversation date from frontmatter, not file mtime — mtime tracks
    when the digest was last (re)indexed, which is unrelated to when the
    conversation happened. Skips files we can't parse: better to keep a digest
    than risk losing one to a parse error.
    """
    cutoff = (datetime.now() - timedelta(days=days_to_keep)).strftime("%Y-%m-%d")
    removed = 0
    for f in DIGEST_DIR.glob("*.md"):
        if f.name == "INDEX.md":
            continue
        try:
            text = f.read_text()
        except OSError:
            continue
        digest_date = None
        in_frontmatter = False
        for line in text.split("\n"):
            stripped = line.strip()
            if stripped == "---":
                if not in_frontmatter:
                    in_frontmatter = True
                    continue
                break
            if in_frontmatter and line.startswith("date:"):
                digest_date = line.split(":", 1)[1].strip()
                break
        if digest_date and digest_date != "unknown" and digest_date < cutoff:
            try:
                f.unlink()
                removed += 1
            except OSError:
                pass
    return removed


def main():
    force = "--force" in sys.argv
    DIGEST_DIR.mkdir(parents=True, exist_ok=True)

    removed = cleanup_old_digests()
    if removed:
        print(f"Pruned {removed} digests older than {DIGEST_RETENTION_DAYS} days")

    jsonl_files = sorted(JSONL_DIR.glob("*.jsonl"))

    existing = set()
    if not force:
        existing = {p.stem for p in DIGEST_DIR.glob("*.md") if p.name != "INDEX.md"}

    to_process = [jf for jf in jsonl_files if jf.stem not in existing or force]

    print(f"Found {len(jsonl_files)} conversations, {len(to_process)} new to index")

    for jf in to_process:
        try:
            parsed = parse_session(jf)
        except Exception as e:
            print(f"  SKIP {jf.stem}: {e}")
            continue

        if not parsed["user_messages"] and not parsed["title"]:
            continue

        digest_text = build_digest(parsed)
        digest_path = DIGEST_DIR / f"{jf.stem}.md"
        digest_path.write_text(digest_text)

    # Rebuild index from ALL digests
    all_digests_meta = []
    for dp in sorted(DIGEST_DIR.glob("*.md")):
        if dp.name == "INDEX.md":
            continue
        try:
            text = dp.read_text()
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
                        val = [a.strip() for a in val.strip("[]").split(",") if a.strip()]
                    meta[key] = val
            all_digests_meta.append(meta)
        except Exception:
            continue

    index_text = build_index(all_digests_meta)
    INDEX_FILE.write_text(index_text)

    print(f"Indexed {len(all_digests_meta)} conversations → {DIGEST_DIR}")


if __name__ == "__main__":
    main()
