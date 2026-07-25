#!/usr/bin/env python3
"""seat_tool — one-command model-slot rotation for the MANIFOLD fleet (BUG-iuf).

Mechanism source of truth: ~/.config/cc-fleet/providers.toml [kimi-code] —
cc-fleet regenerates ~/.claude/profiles/*.json from it and WIPES hand edits,
so profiles are never edited directly.

  seat_tool show                     current slot map from every consumer
  seat_tool assign <slot> <model>    rotate haiku|sonnet|opus to <model>

assign updates providers.toml, runs `cc-fleet repair`, verifies the
regenerated profile, updates the naming-guard slot map, then prints the
remaining manual follow-ups (AGENT_ROUTING table, seat-proxy memory) — prose
docs stay human-edited by design.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

PROVIDERS = Path.home() / ".config/cc-fleet/providers.toml"
PROFILE = Path.home() / ".claude/profiles/kimi-code.json"
LITELLM_CONFIG = Path.home() / ".config/litellm/config.yaml"
REPO = Path(__file__).resolve().parent.parent
NAMING_GUARD = REPO / ".claude/hooks/agent-teammate-naming-guard.py"
TIER_GUARD = REPO / ".claude/hooks/agent-tier-spawn-guard.py"

SLOT_TO_TOML_KEY = {"haiku": "fast_model", "sonnet": "default_model", "opus": "strong_model"}
SLOT_TO_ENV = {
    "haiku": "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "sonnet": "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "opus": "ANTHROPIC_DEFAULT_OPUS_MODEL",
}
# Lane-name labels for the naming guard. Fallback: last dash segment — ugly;
# extend this map when onboarding a model.
SHORT_LABEL = {
    "deepseek-v4-flash": "flash",
    "deepseek-v4-pro": "pro",
    "glm-4.7": "glm47",
    "glm-5.2": "glm52",
    "k3": "k3",
    "kimi-for-coding": "k27",
}


def die(msg):
    sys.exit(f"seat_tool: {msg}")


def read_slot_from_providers(slot):
    text = PROVIDERS.read_text()
    m = re.search(r"\[kimi-code\](.*?)(?=\n\[|\Z)", text, re.DOTALL)
    body = m.group(1)
    km = re.search(rf'^{SLOT_TO_TOML_KEY[slot]}\s*=\s*"([^"]+)"', body, re.MULTILINE)
    return km.group(1) if km else None


def assign(slot, model):
    if slot == "fable":
        die("fable is the lead slot — welded to k3 via the tmux binding, never rotated here")

    # 1. providers.toml
    text = PROVIDERS.read_text()
    key = SLOT_TO_TOML_KEY[slot]
    new, n = re.subn(
        rf'(\[kimi-code\].*?^){key}\s*=\s*"[^"]+"',
        lambda m: f'{m.group(1)}{key} = "{model}"',
        text,
        count=1,
        flags=re.DOTALL | re.MULTILINE,
    )
    if n != 1:
        die(f"{key} not found under [kimi-code] in {PROVIDERS}")
    PROVIDERS.write_text(new)
    print(f"providers.toml [kimi-code] {key} = {model}")

    # 2. regenerate + verify profile
    r = subprocess.run(["cc-fleet", "repair"], capture_output=True, text=True)
    if r.returncode != 0:
        die(f"cc-fleet repair failed: {r.stderr.strip()}")
    env = json.loads(PROFILE.read_text())["env"]
    actual = env[SLOT_TO_ENV[slot]]
    if actual != model:
        die(f"profile verify failed: {SLOT_TO_ENV[slot]} = {actual!r}, expected {model!r}")
    print(f"profile verified: {SLOT_TO_ENV[slot]} = {model}")

    # 3. naming-guard slot map
    label = SHORT_LABEL.get(model)
    if not label:
        label = re.sub(r"[^a-z0-9]", "", model.split("-")[-1].lower())
        print(f"WARNING: no short label for {model!r}; using {label!r} — extend SHORT_LABEL")
    guard = NAMING_GUARD.read_text()
    new_guard, n = re.subn(
        rf'("{slot}":\s*")[^"]+(")',
        rf"\g<1>{label}\g<2>",
        guard,
        count=1,
    )
    if n != 1:
        die(f"SLOT_FOR_MODEL[{slot!r}] not found in {NAMING_GUARD}")
    NAMING_GUARD.write_text(new_guard)
    print(f"naming-guard SLOT_FOR_MODEL[{slot!r}] = {label!r}")

    # 4. warnings: litellm must serve it; tier guard must classify it
    if f"model_name: {model}" not in LITELLM_CONFIG.read_text():
        print(f"WARNING: {model!r} not in litellm model_list — proxy cannot serve it yet")
    tier_src = TIER_GUARD.read_text()
    exe = re.search(r'EXECUTOR_TIERS\s*=\s*re\.compile\(\s*r"([^"]+)"', tier_src).group(1)
    dsp = re.search(r'DISPATCHER_TIERS\s*=\s*re\.compile\(\s*r"([^"]+)"', tier_src).group(1)
    want = dsp if slot == "opus" else exe
    if not re.search(want, model, re.IGNORECASE):
        which = "DISPATCHER_TIERS" if slot == "opus" else "EXECUTOR_TIERS"
        print(f"WARNING: {model!r} not matched by {which} in agent-tier-spawn-guard.py — edit the regex")

    print(f"""
done. Manual follow-ups (prose stays human-edited):
  - docs/AGENT_ROUTING.md slot-map table row for \"{slot}\"
  - memory reference_litellm_seat_proxy (slot line) — pointers only, no status
  - takes effect on next lead-session start (running sessions keep old env)""")


def show():
    env = json.loads(PROFILE.read_text())["env"]
    guard = NAMING_GUARD.read_text()
    served = LITELLM_CONFIG.read_text()
    print(f"{'slot':8} {'providers.toml':22} {'profile env':22} {'lane label':10} served?")
    for slot in SLOT_TO_TOML_KEY:
        toml_val = read_slot_from_providers(slot) or "—"
        env_val = env.get(SLOT_TO_ENV[slot], "—")
        m = re.search(rf'"{slot}":\s*"([^"]+)"', guard)
        label = m.group(1) if m else "—"
        yes = "yes" if f"model_name: {toml_val}" in served else "NO"
        flag = "" if toml_val == env_val else "  <- DRIFT"
        print(f"{slot:8} {toml_val:22} {env_val:22} {label:10} {yes}{flag}")


if __name__ == "__main__":
    args = sys.argv[1:]
    if args == ["show"]:
        show()
    elif len(args) == 3 and args[0] == "assign" and args[1] in SLOT_TO_TOML_KEY:
        assign(args[1], args[2])
    else:
        die("usage: seat_tool show | seat_tool assign <haiku|sonnet|opus> <model>")
