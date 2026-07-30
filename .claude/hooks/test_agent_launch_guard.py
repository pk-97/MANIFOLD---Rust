#!/usr/bin/env python3
"""Standalone test runner for agent-launch-guard.py.

Invokes decide()/backend_for_slot() directly with synthetic input — never
spawns a real hook subprocess against a live session.

Run: python3 .claude/hooks/test_agent_launch_guard.py
"""
import importlib.util
from pathlib import Path

HOOKS_DIR = Path(__file__).resolve().parent
HOOK_PATH = HOOKS_DIR / "agent-launch-guard.py"
spec = importlib.util.spec_from_file_location("launch_guard", HOOK_PATH)
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

# --- decide(): one-pass model + naming -------------------------------------


def spawn(name=None, subagent_type="general-purpose", env=SEAT_ENV, **kw):
    ti = {"subagent_type": subagent_type, **kw}
    if name is not None:
        ti["name"] = name
    return hook.decide(ti, env)


# Clean launches.
check("good name allowed", spawn("flash-beads-migration", model="haiku") == ("", ""))
check("good sonnet name allowed", spawn("glm47-doc-sweep", model="sonnet") == ("", ""))
check("nameless with model allowed", spawn(model="haiku") == ("", ""))
check("fork exempt even without model", hook.decide({"subagent_type": "fork"}, SEAT_ENV) == ("", ""))
check("missing subagent_type allowed", spawn("flash-beads-migration", model="haiku", subagent_type="") == ("", ""))

# opus/fable: allowed with reminder note.
d, n = spawn("glm52-doc-sweep", model="opus")
check("explicit opus allowed with reminder", d == "" and "deliberate" in n)
d, n = spawn(model="fable")
check("explicit fable nameless allowed with reminder", d == "" and "Sonnet" in n)

# Missing model alone -> one deny, corrected call uses house default.
d, _ = spawn(model=None)
check("missing model denied", "2026-07-06" in d and 'model="sonnet"' in d)
check("missing model deny is single-defect", "1 defect " in d)

# THE incident class (2026-07-30): missing model AND unprefixed name must be
# ONE deny that spells the complete corrected call.
d, _ = spawn("wr-live-status-lane")
check(
    "both defects in one deny",
    "2 defects" in d and "2026-07-06" in d and "slot prefix" in d,
)
check(
    "combined deny spells full corrected call",
    'model="sonnet"' in d and 'name="glm47-wr-live-status-lane"' in d,
)

# Naming defects with an explicit model still deny with the corrected name.
d, _ = spawn("Flash-Beads-Migration", model="haiku")
check("bad casing denied", "kebab-case" in d and 'name="flash-beads-migration"' in d)

d, _ = spawn("worker-beads-migration", model="haiku")
check(
    "unknown slot denied names live map",
    "deepseek-v4-flash" in d and 'name="flash-worker-beads-migration"' in d,
)

d, _ = spawn("glm52-beads-migration", model="haiku")
check(
    "slot/model mismatch denied with correction",
    "glm52" in d and "flash" in d and 'name="flash-beads-migration"' in d,
)

d, _ = spawn("flash-t1", model="haiku")
check(
    "opaque task part denied with placeholder",
    "descriptive" in d and 'name="flash-<two-plain-words>"' in d,
)

# Anthropic path end-to-end: tier names are the slots.
check("anthropic slot name allowed", spawn("sonnet-doc-sweep", model="sonnet", env={}) == ("", ""))
d, _ = spawn("flash-doc-sweep", model="sonnet", env={})
check("anthropic denies provider slot", "sonnet" in d)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURE(S)")
    raise SystemExit(1)
print("ALL PASS")
