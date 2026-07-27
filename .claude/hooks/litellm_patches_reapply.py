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
    ANTHROPIC_API_KEY=$(cc-fleet keyget kimi) \
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
    # --- reasoning-drop patch set (2026-07-24, glm-4.7 lead): opencode
    # deepseek-v4-flash reasons UNCONDITIONALLY — reasoning-off request params
    # (`reasoning: {enabled:false}`, `effort:"none"`) are ignored upstream
    # (verified by direct probe), so every Flash response carries
    # reasoning_content, which this adapter translates into a `thinking`
    # block with an EMPTY signature. The CC harness stream parser rejects it:
    # "API Error: Content block is not a text block" retry storms (26 in one
    # T1 lane transcript). The haiku slot never negotiates thinking, so the
    # correct shape is to DROP reasoning at the bridge: plain text/tool_use
    # only. Reasoning tokens are billed upstream regardless — the drop only
    # removes them from the wire, it does not save tokens.
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/transformation.py",
        "regex": (
            r"            # Handle reasoning_content when thinking_blocks is not present\n"
            r"            elif hasattr\(choice\.message, \"reasoning_content\"\) and choice\.message\.reasoning_content:\n"
            r"                new_content\.append\(\n"
            r"                    AnthropicResponseContentBlockThinking\(\n"
            r"                        type=\"thinking\",\n"
            r"                        thinking=str\(choice\.message\.reasoning_content\),\n"
            r"                        signature=None,\n"
            r"                    \)\.model_dump\(\)\n"
            r"                \)\n"
        ),
        "replace": (
            "            # MANIFOLD local patch: upstream reasoning_content dropped, not\n"
            "            # translated to a thinking block (see the streaming-delta site\n"
            "            # below for the full rationale). Non-streaming mirror.\n"
        ),
    },
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/transformation.py",
        "regex": (
            r"            elif isinstance\(choice, StreamingChoices\) and getattr\(choice\.delta, \"reasoning_content\", None\):\n"
            r"                return \"thinking\", ChatCompletionThinkingBlock\(type=\"thinking\", thinking=\"\", signature=\"\"\)\n"
        ),
        "replace": (
            "            # MANIFOLD local patch: reasoning_content no longer opens a\n"
            "            # thinking block — falls through to the plain text block below\n"
            "            # (rationale at the streaming-delta site).\n"
        ),
    },
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/transformation.py",
        "regex": (
            r"            # Handle reasoning_content when thinking_blocks is not present\n"
            r"            # This handles providers like OpenRouter that return reasoning_content\n"
            r"            elif isinstance\(choice, StreamingChoices\) and hasattr\(choice\.delta, \"reasoning_content\"\):\n"
            r"                if choice\.delta\.reasoning_content is not None:\n"
            r"                    reasoning_content \+= choice\.delta\.reasoning_content\n"
        ),
        "replace": (
            "            # MANIFOLD local patch: upstream reasoning_content DROPPED, not\n"
            "            # accumulated into thinking deltas — opencode deepseek-v4-flash\n"
            "            # reasons unconditionally (reasoning-off params ignored upstream,\n"
            "            # verified by direct probe 2026-07-24) and the translated thinking\n"
            "            # block (empty signature) trips the CC harness stream parser:\n"
            "            # \"API Error: Content block is not a text block\" retry storms in\n"
            "            # Flash lanes. Dropping yields plain text/tool_use — the\n"
            "            # non-thinking shape the haiku slot expects. Reasoning tokens are\n"
            "            # billed upstream regardless; the drop only strips them off the wire.\n"
            "            # NOTE: the pre-translation strip in streaming_iterator.py is the\n"
            "            # primary guard (empty text_deltas on tool_use blocks); this site\n"
            "            # is defense in depth for paths that bypass the iterator.\n"
        ),
    },
    # --- reasoning strip pre-translation (2026-07-24, glm-4.7 lead, second
    # round): the transformation.py drop above still emits an EMPTY text_delta
    # for each pure-reasoning chunk, and DeepSeek streams reasoning AFTER tool
    # call deltas — so the empty text_delta lands on the open tool_use block
    # and the harness errors "Content block is not a text block" on every
    # tool-using turn (confirmed by SSE capture). Fix at the iterator: strip
    # reasoning_content from each chunk BEFORE translation and skip chunks
    # that carry nothing else (finish_reason/usage chunks always pass).
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/streaming_iterator.py",
        # anchor = the module-level import just above the insertion point
        # (insert lands between it and the TYPE_CHECKING block / first class).
        "anchor": "from litellm.types.utils import AdapterCompletionStreamWrapper\n",
        "insert": (
            "\n\ndef _manifold_reasoning_only_chunk(chunk) -> bool:\n"
            "    # MANIFOLD local patch: strip upstream reasoning deltas BEFORE\n"
            "    # translation. opencode deepseek-v4-flash reasons unconditionally and\n"
            "    # streams reasoning AFTER tool-call deltas; translated to empty\n"
            "    # text_delta events those land on the open tool_use block and the CC\n"
            "    # harness parser errors \"Content block is not a text block\" (every\n"
            "    # tool-using turn, 2026-07-24). Returns True when the chunk carries\n"
            "    # ONLY reasoning (skip it); reasoning is also stripped in place from\n"
            "    # chunks that must pass (finish_reason / usage / content / tool_calls).\n"
            "    if not getattr(chunk, \"choices\", None):\n"
            "        return False\n"
            "    reasoning_only = True\n"
            "    for _choice in chunk.choices:\n"
            "        if getattr(_choice, \"finish_reason\", None):\n"
            "            reasoning_only = False\n"
            "        _delta = getattr(_choice, \"delta\", None)\n"
            "        if _delta is None:\n"
            "            continue\n"
            "        if getattr(_delta, \"reasoning_content\", None):\n"
            "            try:\n"
            "                _delta.reasoning_content = None\n"
            "            except Exception:\n"
            "                pass\n"
            "        if getattr(_delta, \"content\", None):\n"
            "            reasoning_only = False\n"
            "        if getattr(_delta, \"tool_calls\", None):\n"
            "            reasoning_only = False\n"
            "    return reasoning_only\n"
            "\n\n"
        ),
    },
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/streaming_iterator.py",
        "regex": (
            r"(                if chunk == \"None\" or chunk is None:\n"
            r"                    raise Exception\n)"
            r"(?!                # MANIFOLD local patch: skip pure-reasoning)"
        ),
        "replace": (
            "\\1"
            "                # MANIFOLD local patch: skip pure-reasoning chunks (see\n"
            "                # _manifold_reasoning_only_chunk above).\n"
            "                if _manifold_reasoning_only_chunk(chunk):\n"
            "                    continue\n"
        ),
    },
    {
        # Round 3 (2026-07-24): the residual stray delta. opencode emits
        # empty-choices chunks with NO usage field (`x-opencode-type:
        # inference-cost` carrying `normalizedUsage`, plus a trailing
        # `{"choices":[],"cost":"0"}` after [DONE]) — these never enter the
        # usage-merge path (which keys on chunk.usage), so they hit the D-48
        # keepalive translation and emit an empty text_delta against whatever
        # block is current: after a tool call, the CLOSED tool_use block →
        # harness "Content block is not a text block" on every tool turn.
        # Skip empty-choices chunks unless they carry a usage to merge.
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/streaming_iterator.py",
        "regex": (
            r"    if not getattr\(chunk, \"choices\", None\):\n"
            r"        return False\n"
        ),
        "replace": (
            "    if not getattr(chunk, \"choices\", None):\n"
            "        # MANIFOLD local patch: empty-choices keepalive/cost chunks with\n"
            "        # no usage to merge (opencode inference-cost / post-[DONE] noise)\n"
            "        # translate to an empty text_delta on the CURRENT block — after a\n"
            "        # tool call that is a closed tool_use block, a protocol violation\n"
            "        # the CC harness rejects. Skip them; usage-carrying chunks pass\n"
            "        # through to the merge path.\n"
            "        return getattr(chunk, \"usage\", None) is None\n"
        ),
    },
    # --- json_schema downgrade (2026-07-25, k3 lead): Claude Code's haiku-slot
    # session-title call sends output_config.format = json_schema; this adapter
    # translates it to OpenAI-style response_format json_schema, which the
    # opencode "Console Go" upstream 400s on — bisected field-by-field from a
    # captured live request (sidecar on :4055): stream/effort/max_tokens all
    # tolerated, json_schema the sole trigger, json_object passes WITH
    # streaming. Result was one failed Flash call per user prompt (18 in 6h)
    # and no session titles. Every CC structured-output prompt carries its own
    # "Return JSON..." instruction, so enforcement-only json_object loses
    # nothing — the schema was never honored upstream anyway.
    {
        "file": "litellm/llms/anthropic/experimental_pass_through/adapters/transformation.py",
        "regex": (
            r"        # Convert to OpenAI response_format structure\n"
            r"        return \{\n"
            r"            \"type\": \"json_schema\",\n"
            r"            \"json_schema\": \{\n"
            r"                \"name\": \"structured_output\",\n"
            r"                \"schema\": schema,\n"
            r"                \"strict\": True,\n"
            r"            \},\n"
            r"        \}\n"
        ),
        "replace": (
            "        # MANIFOLD local patch: downgrade json_schema -> json_object\n"
            "        # (opencode upstream 400s on json_schema response_format;\n"
            "        # full rationale in litellm_patches_reapply.py).\n"
            "        return {\"type\": \"json_object\"}\n"
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