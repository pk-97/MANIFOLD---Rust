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
check("good name allowed", spawn("flash-beads-migration", model="haiku") == ("", "", None))
check("good sonnet name allowed", spawn("glm47-doc-sweep", model="sonnet") == ("", "", None))
check("nameless with model allowed", spawn(model="haiku") == ("", "", None))
check("fork exempt even without model", hook.decide({"subagent_type": "fork"}, SEAT_ENV) == ("", "", None))
check(
    "missing subagent_type allowed",
    spawn("flash-beads-migration", model="haiku", subagent_type="") == ("", "", None),
)

# opus/fable: allowed with reminder note.
d, n, f = spawn("glm52-doc-sweep", model="opus")
check("explicit opus allowed with reminder", d == "" and f is None and "deliberate" in n)
d, n, f = spawn(model="fable")
check("explicit fable nameless allowed with reminder", d == "" and f is None and "Sonnet" in n)

# Mechanical name defects -> AUTO-FIXED via fixed_input, never a deny.
d, n, f = spawn("Flash-Beads-Migration", model="haiku")
check(
    "bad casing auto-fixed",
    d == "" and f is not None and f["name"] == "flash-beads-migration" and "auto-fixed" in n,
)
check("auto-fix preserves other input", f.get("model") == "haiku" and f.get("subagent_type") == "general-purpose")

d, n, f = spawn("worker-beads-migration", model="haiku")
check(
    "missing slot prefix auto-fixed",
    d == "" and f is not None and f["name"] == "flash-worker-beads-migration" and "auto-fixed" in n,
)

d, n, f = spawn("glm52-beads-migration", model="haiku")
check(
    "slot/model mismatch auto-fixed to model's slot",
    d == "" and f is not None and f["name"] == "flash-beads-migration",
)

d, n, f = spawn("Scene_Loop Wrap!!", model="sonnet")
check(
    "chars + casing + prefix all auto-fixed in one pass",
    d == "" and f is not None and f["name"] == "glm47-scene-loop-wrap",
)

# Judgment defects still deny: opaque task part, missing model.
d, n, f = spawn("flash-t1", model="haiku")
check(
    "opaque task part denied with placeholder",
    f is None and "descriptive" in d and 'name="flash-<two-plain-words>"' in d,
)

d, n, f = spawn(model=None)
check("missing model denied", f is None and "2026-07-06" in d and 'model="sonnet"' in d)
check("missing model deny is single-defect", "1 defect " in d)

# Missing model AND unprefixed name: the name fix is mechanical (spelled in the
# corrected call), the model is the one judgment defect — ONE deny.
d, n, f = spawn("wr-live-status-lane")
check(
    "missing model is the single defect",
    f is None and "1 defect " in d and "2026-07-06" in d,
)
check(
    "combined deny spells full corrected call",
    'model="sonnet"' in d and 'name="glm47-wr-live-status-lane"' in d,
)

# Anthropic path end-to-end: tier names are the slots.
check("anthropic slot name allowed", spawn("sonnet-doc-sweep", model="sonnet", env={}) == ("", "", None))
d, n, f = spawn("flash-doc-sweep", model="sonnet", env={})
check(
    "anthropic provider slot auto-fixed to tier name",
    d == "" and f is not None and f["name"] == "sonnet-flash-doc-sweep",
)

print()
if FAILURES:
    print(f"{len(FAILURES)} FAILURE(S)")
    raise SystemExit(1)
print("ALL PASS")
