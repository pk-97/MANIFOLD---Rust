#!/usr/bin/env python3
"""flash_api.py — DeepSeek Flash as a semantic tool, not an agent (D-55/R6-B).

One raw completion through the litellm proxy (metered like every other Flash
call) for tasks that need a language model but NOT a harness session:
formatting a record from substance you dictate, read-and-quote lookups,
summarising a lane report. The lead/dispatcher stays out of the loop's
context; the call is one request, one response, no tools, no retry storm.

RULES (from tonight's findings):
- You supply the substance; Flash formats or fetches. Never ask it for
  judgment — its self-reports are unreliable (T2 lesson).
- Never let it write the audit trail unsupervised (decisions, backlog
  entries): those are the record, the lead owns their content.
- Reasoning is dropped at the proxy (D-54): you get plain text back.

Lives in .claude/scripts/ because that is the versioned, main-editable
.claude dir (same precedent as litellm_patches_reapply.py) — an operational
tool, not app code, so no worktree slot needed.

Usage:
    python3 .claude/scripts/flash_api.py "prompt text"
    python3 .claude/scripts/flash_api.py --file brief.txt "extra instruction"
    echo "text" | python3 .claude/scripts/flash_api.py --stdin "format as ..."

Exit codes: 0 = completion printed to stdout; 1 = proxy/HTTP error (body to
stderr); 2 = usage error.
"""
import json
import os
import sys
import urllib.request

PROXY = "http://127.0.0.1:4000/v1/chat/completions"
KEY_FILES = [
    os.path.expanduser("~/.config/litellm/key-flash-executor.json"),
    os.path.expanduser("~/.config/litellm/key-k3-lead.json"),
]
MODEL = "deepseek-v4-flash"
MAX_TOKENS = 4096
TIMEOUT_S = 120


def load_key() -> str:
    for path in KEY_FILES:
        try:
            with open(path) as f:
                data = json.load(f)
        except (OSError, ValueError):
            continue
        for field in ("key", "token", "api_key"):
            value = data.get(field)
            if isinstance(value, str) and value.startswith("sk-"):
                return value
    sys.exit("flash_api: no usable litellm virtual key in " + ", ".join(KEY_FILES))


def main() -> None:
    args = sys.argv[1:]
    parts = []
    i = 0
    while i < len(args):
        if args[i] == "--file" and i + 1 < len(args):
            try:
                with open(args[i + 1]) as f:
                    parts.append(f.read())
            except OSError as e:
                sys.exit(f"flash_api: {e}")
            i += 2
        elif args[i] == "--stdin":
            parts.append(sys.stdin.read())
            i += 1
        else:
            parts.append(args[i])
            i += 1
    prompt = "\n\n".join(p for p in parts if p)
    if not prompt.strip():
        sys.exit("usage: flash_api.py [--file path] [--stdin] \"prompt\"")

    body = json.dumps(
        {
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "messages": [{"role": "user", "content": prompt}],
        }
    ).encode()
    req = urllib.request.Request(
        PROXY,
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {load_key()}",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
            data = json.load(resp)
    except Exception as e:
        detail = getattr(e, "read", lambda: b"")()
        sys.stderr.write(detail.decode(errors="replace")[:500] if detail else f"{e}\n")
        sys.exit(1)
    try:
        print(data["choices"][0]["message"]["content"])
    except (KeyError, IndexError, TypeError):
        sys.stderr.write(json.dumps(data)[:500] + "\n")
        sys.exit(1)


if __name__ == "__main__":
    main()
