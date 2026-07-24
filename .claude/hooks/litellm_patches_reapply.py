#!/usr/bin/env python3
"""Reapply the MANIFOLD local patches to the litellm venv (D-48, 2026-07-24).

Why: litellm 1.93.0 (latest at patch time) crashes on opencode's streaming
keepalive chunks — empty `choices` arrays in the anthropic<->openai
translation path. DeepSeek Flash lanes ride that path (opencode Go speaks
OpenAI protocol only), so an unpatched venv silently kills the executor
tier. pip upgrades REVERT these patches; this script re-applies them
idempotently and verifies.

Run after ANY litellm upgrade:
    python3 .claude/hooks/litellm_patches_reapply.py
(Lives in hooks/ because that is the versioned, main-editable .claude dir —
it is an operational tool, not a Claude Code hook.)
    launchctl kickstart -k gui/$(id -u)/com.manifold.litellm-proxy

Canary (proves the whole route, not just the patch):
    ANTHROPIC_BASE_URL=http://127.0.0.1:4000 \
    ANTHROPIC_API_KEY=$(cc-fleet keyget kimi-code) \
    claude -p "Reply with exactly: REAL-CC-OK" --model deepseek-v4-flash --max-turns 1

Also load-bearing, but config-side (survives upgrades, listed for the map):
    ~/.config/litellm/config.yaml
      use_chat_completions_url_for_anthropic_messages: true
    (opencode's /responses endpoint is nonstandard; chat-completions works)

Upstream: worth filing at github.com/BerriAI/litellm — unguarded
chunk.choices[0] on empty-choices streaming chunks. Delete this directory
once an upgraded litellm passes the canary unpatched.
"""
import re
import sys
from pathlib import Path

VENV = Path.home() / ".local/litellm-venv/lib"
MARKER = "MANIFOLD local patch"

PATCHES = [
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/streaming_iterator.py",
        "anchor": (
            "        # Example logic - customize based on your needs:\n"
            "        # If chunk indicates a tool call\n"
        ),
        "insert": (
            "        # MANIFOLD local patch: some openai-compatible upstreams (opencode\n"
            '        # "Console Go") emit keepalive/usage chunks with an empty choices\n'
            "        # array; unguarded [0] access crashes the whole stream.\n"
            "        if not chunk.choices:\n"
            "            return False\n"
        ),
    },
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/streaming_iterator.py",
        "regex": (
            r"is_final_chunk = chunk\.choices\[0\]\.finish_reason is not None(?!  # MANIFOLD)"
        ),
        "replace": (
            "is_final_chunk = bool(chunk.choices) and "
            "chunk.choices[0].finish_reason is not None"
            "  # MANIFOLD local patch: empty-choices keepalive chunks"
        ),
    },
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/transformation.py",
        "anchor": "        ## base case - final chunk w/ finish reason\n",
        "insert": (
            '        # MANIFOLD local patch: opencode "Console Go" emits keepalive/usage\n'
            "        # chunks with empty choices; treat them as a contentless text delta.\n"
            "        if not response.choices:\n"
            "            return ContentBlockDelta(\n"
            "                type=\"content_block_delta\",\n"
            "                index=current_content_block_index,\n"
            "                delta=ContentTextBlockDelta(type=\"text_delta\", text=\"\"),\n"
            "            )\n"
        ),
    },
]


def find_target(rel: str) -> Path:
    hits = sorted(VENV.glob(f"python*/site-packages/{rel}"))
    if not hits:
        sys.exit(f"MISSING: {rel} — litellm venv layout changed; patch by hand.")
    return hits[0]


def main() -> None:
    changed = 0
    for p in PATCHES:
        path = find_target(p["file"])
        src = path.read_text()
        if "regex" in p:
            new, n = re.subn(p["regex"], p["replace"], src)
            if n:
                path.write_text(new)
                changed += n
                print(f"patched ({n} site(s)): {path.name} [regex]")
            else:
                print(f"already applied or upstream-fixed: {path.name} [regex]")
            continue
        if p["insert"] in src:
            print(f"already applied: {path.name}")
            continue
        if p["anchor"] not in src:
            sys.exit(
                f"ANCHOR MISSING in {path} — upstream refactored; re-derive the "
                "patch (bug: unguarded chunk.choices[0] on empty-choices chunks) "
                "or verify the canary passes unpatched."
            )
        path.write_text(src.replace(p["anchor"], p["anchor"] + p["insert"], 1))
        changed += 1
        print(f"patched: {path.name}")
    print(f"\ndone — {changed} change(s). Restart the proxy, then run the canary "
          "(header of this file).")


if __name__ == "__main__":
    main()