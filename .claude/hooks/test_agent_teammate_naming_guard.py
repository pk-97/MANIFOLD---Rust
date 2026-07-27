#!/usr/bin/env python3
"""Standalone test runner for agent-teammate-naming-guard.py.

Invokes decide()/backend_for_slot() directly with synthetic input — never
spawns a real hook subprocess against a live session.

Run: python3 .claude/hooks/test_agent_teammate_naming_guard.py
"""
import importlib.util
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
HOOK_PATH = HOOKS_DIR / "agent-teammate-naming-guard.py"
spec = importlib.util.spec_from_file_location("naming_guard", HOOK_PATH)
hook = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hook)

FAILURES = []


def check(name: str, cond: bool) -> None:
    print(("PASS " if cond else "FAIL ") + name)
    if not cond:
        FAILURES.append(name)


# Provider-seat env (D-48 slot map shape, sonnet on glm-4.7 per 2026-07-27).
SEAT_ENV = {
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "deepseek-v4-flash",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "glm-4.7",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "glm-5.2",
    "ANTHROPIC_DEFAULT_FABLE_MODEL": "k3",
}

# --- backend_for_slot: env-derived resolution ------------------------------

check("haiku -> flash", hook.backend_for_slot("haiku", SEAT_ENV) == ("deepseek-v4-flash", "flash"))
check("sonnet -> glm47", hook.backend_for_slot("sonnet", SEAT_ENV) == ("glm-4.7", "glm47"))
check("opus -> glm52", hook.backend_for_slot("opus", SEAT_ENV) == ("glm-5.2", "glm52"))
check("fable -> k3", hook.backend_for_slot("fable", SEAT_ENV) == ("k3", "k3"))

# Anthropic path: env unset or claude-* -> label is the tier name itself.
check("anthropic unset -> tier name", hook.backend_for_slot("sonnet", {}) == ("sonnet", "sonnet"))
check(
    "anthropic claude-* -> tier name",
    hook.backend_for_slot("haiku", {"ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4-5-20251001"})
    == ("claude-haiku-4-5-20251001", "haiku"),
)

# Unknown backend -> last-segment fallback label.
check(
    "unknown backend fallback",
    hook.backend_for_slot("haiku", {"ANTHROPIC_DEFAULT_HAIKU_MODEL": "qwen3-coder"})[1] == "coder",
)

# --- decide(): naming rules against the live map ---------------------------


def spawn(name, model="haiku", subagent_type="general-purpose", env=SEAT_ENV):
    return hook.decide({"name": name, "model": model, "subagent_type": subagent_type}, env)


check("good name allowed", spawn("flash-beads-migration") == "")
check("good sonnet name allowed", spawn("glm47-doc-sweep", model="sonnet") == "")
check("fork exempt", hook.decide({"subagent_type": "fork"}, SEAT_ENV) == "")
check("nameless allowed", hook.decide({"model": "haiku", "subagent_type": "general-purpose"}, SEAT_ENV) == "")

r = spawn("flash-beads-migration", subagent_type="")
check("missing subagent_type denied", "subagent_type" in r)

r = spawn("Flash-Beads-Migration")
check("bad casing denied", "kebab-case" in r)

r = spawn("worker-beads-migration")
check("unknown slot denied names live map", "worker" in r and "deepseek-v4-flash" in r)

r = spawn("glm52-beads-migration", model="haiku")
check(
    "slot/model mismatch denied with backend",
    "glm52" in r and "deepseek-v4-flash" in r and "flash-beads-migration" in r,
)

r = spawn("flash-t1")
check("opaque task part denied", "not" in r and "descriptive" in r)

# Anthropic path end-to-end: tier names are the slots.
check("anthropic slot name allowed", spawn("sonnet-doc-sweep", model="sonnet", env={}) == "")
r = spawn("flash-doc-sweep", model="sonnet", env={})
check("anthropic denies provider slot", "sonnet" in r)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURE(S)")
    raise SystemExit(1)
print("ALL PASS")
