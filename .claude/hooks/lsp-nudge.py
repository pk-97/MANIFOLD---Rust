#!/usr/bin/env python3
"""PreToolUse(Bash) nudge: redirect symbol-shaped rg/grep to the LSP tool.

Why this exists: Claude Code's system prompt biases the model toward grep, and a
passive "prefer LSP" line in CLAUDE.md loses to it (~1% real-world LSP use). The
reliable redirect is a *narrow soft-block*: when a search is clearly a Rust
symbol/definition/impl query, deny it with a reason that names the LSP operation
to use instead. The model sees the deny reason and adapts.

Deliberately narrow to keep false positives near zero — it fires ONLY on
definition-shaped patterns (`fn name`, `trait name`, `struct name`, `enum name`)
and trait-impl shapes (`impl ... for`). A bare keyword (`rg "trait"`), a plain
identifier, a string/JSON/log/doc search — all pass untouched.

Bypass: append `#grep-ok` to the command to force the text search through. The
cost of a false positive is therefore one of: re-issue as an LSP call (the point),
or re-run with `#grep-ok`.

Runs as a second PreToolUse Bash hook alongside preToolUseBash.py. A `deny` here
overrides that hook's `allow` (deny takes precedence across hooks).

Receives {"tool_name": "Bash", "tool_input": {"command": "..."}} on stdin.
Emits hookSpecificOutput.permissionDecision="deny" + reason, or nothing.
"""
import json
import re
import sys

try:
    data = json.load(sys.stdin)
except Exception:
    sys.exit(0)

cmd = ((data.get("tool_input") or {}).get("command") or "")
if not cmd:
    sys.exit(0)

# Explicit bypass — the model meant a text search.
if "#grep-ok" in cmd:
    sys.exit(0)

# Only consider commands that actually run a text searcher.
if not re.search(r"\b(rg|grep|egrep|fgrep|ack|ag)\b", cmd):
    sys.exit(0)

# Trait-impl shape -> goToImplementation.
impl_shape = re.search(r"\bimpl\b.*\bfor\b", cmd) or re.search(r"\bimpl\s+[A-Za-z_]", cmd)
# Definition shape -> workspaceSymbol / goToDefinition.
def_shape = re.search(r"\b(fn|trait|struct|enum)\s+[A-Za-z_]", cmd)

if not (impl_shape or def_shape):
    sys.exit(0)

if impl_shape:
    op = "goToImplementation on the trait (lists every implementor across crates) — or findReferences"
else:
    op = "workspaceSymbol by name, or goToDefinition on a use site (documentSymbol for a single file)"

reason = (
    "Symbol-shaped search detected. The LSP tool (rust-analyzer) is more reliable here "
    "than text grep: no false hits from comments/strings/same-named methods, and it "
    "follows trait dispatch and re-exports across crates.\n"
    f"Use the LSP tool: {op}.\n"
    "If you genuinely want a TEXT search (strings, JSON, logs, comments, a .rs literal), "
    "re-run the SAME command with `#grep-ok` appended to bypass this."
)

print(json.dumps({
    "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }
}))
sys.exit(0)
