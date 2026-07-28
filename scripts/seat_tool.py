#!/usr/bin/env python3
"""seat_tool — one command surface for the MANIFOLD fleet's seat plumbing.

Source of truth for seat NAMES: scripts/fleet_seats.toml (a seat = one
subscription account, never a model). Source of truth for slot MODELS:
~/.config/cc-fleet/providers.toml [<lead_seat>] — cc-fleet regenerates
~/.claude/profiles/*.json from it and WIPES hand edits, so profiles are never
edited directly (drift incident BUG-iuf, seat rotation tooling).

  seat_tool show                     current slot map from every consumer
  seat_tool assign <slot> <model>    rotate haiku|sonnet|opus to <model>
  seat_tool check                    verify every consumer against the manifest
  seat_tool rename <old> <new> [--env-old KEY]
                                     migrate a seat name across every consumer

rename rewrites seat tokens in all registered mechanism files (home configs +
repo hooks + the two fleet docs), renames the secret files, deletes stale
profiles, and runs `cc-fleet repair`. Token matching is word-boundary strict:
`glm` the seat never matches `glm-4.7` the model, `%glm%` SQL patterns, or
`glm*` shell globs. After renaming every seat: run `check`, restart the proxy
(env var names move with --env-old), canary one subagent per seat. check is
the drift gate — hand edits or upgrades that resurrect a retired name fail
loudly here.
"""
import json
import re
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / "scripts/fleet_seats.toml"
HOME = Path.home()
PROVIDERS = HOME / ".config/cc-fleet/providers.toml"
SECRETS = HOME / ".config/cc-fleet/secrets"
PROFILES = HOME / ".claude/profiles"
START_PROXY = HOME / ".config/litellm/start-proxy.sh"
LITELLM_CONFIG = HOME / ".config/litellm/config.yaml"
STATUSLINE = HOME / ".claude/statusline.sh"
ZSHRC = HOME / ".zshrc"
TMUX_CONF = HOME / ".tmux.conf"

HOOKS = REPO / ".claude/hooks"
SEAT_IDENTITY = HOOKS / "seat-identity.py"
TIER_GUARD_CCFLEET = HOOKS / "cc-fleet-tier-guard.py"
NAMING_GUARD = HOOKS / "agent-teammate-naming-guard.py"
TIER_GUARD_SPAWN = HOOKS / "agent-tier-spawn-guard.py"

# Every file that may carry seat-name tokens. rename rewrites these; check
# scans this mechanism set for retired names. Docs are renamed too but not
# retired-checked — prose legitimately mentions model families.
MECHANISM_FILES = [
    PROVIDERS, START_PROXY, LITELLM_CONFIG, STATUSLINE, ZSHRC,
    SEAT_IDENTITY, TIER_GUARD_CCFLEET,
    HOOKS / "oneshot", HOOKS / "litellm_patches_reapply.py",
    HOOKS / "preToolUseBash.py",
    HOOKS / "test_cc_fleet_tier_guard.py", HOOKS / "test_preToolUseBash.py",
]
DOC_FILES = [REPO / "docs/AGENT_ROUTING.md", REPO / "docs/PROVIDER_OPERATIONS.md"]

SLOT_TO_TOML_KEY = {"haiku": "fast_model", "sonnet": "default_model", "opus": "strong_model"}
SLOT_TO_ENV = {
    "haiku": "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "sonnet": "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "opus": "ANTHROPIC_DEFAULT_OPUS_MODEL",
}


def token_re(name):
    """A seat token never abuts word chars, '-', '.', '%' or '*' — shields
    model names (glm-4.7), globs (glm*), SQL patterns (%glm%), filenames."""
    return re.compile(rf"(?<![\w.%*-]){re.escape(name)}(?![\w.%*-])")


def die(msg):
    sys.exit(f"seat_tool: {msg}")


def load_manifest():
    return tomllib.loads(MANIFEST.read_text())


def load_naming_guard():
    import importlib.util
    spec = importlib.util.spec_from_file_location("naming_guard", NAMING_GUARD)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---------------------------------------------------------------- check

def check():
    m = load_manifest()
    lead = m["lead_seat"]
    fails = []

    def need(cond, what):
        if not cond:
            fails.append(what)

    providers = PROVIDERS.read_text()
    statusline = STATUSLINE.read_text()
    proxy_sh = START_PROXY.read_text()
    litellm = LITELLM_CONFIG.read_text()
    identity = SEAT_IDENTITY.read_text()

    for row, seat in m["seat"].items():
        need(f"[{row}]" in providers, f"providers.toml: missing [{row}]")
        need(f"[{row}-upstream]" in providers, f"providers.toml: missing [{row}-upstream]")
        need(f'secret_ref = "{row}.key"' in providers,
             f"providers.toml: [{row}] secret_ref != {row}.key")
        need((SECRETS / f"{row}.key").exists(), f"secrets: {row}.key missing")
        need((SECRETS / f"{row}-upstream.key").exists(), f"secrets: {row}-upstream.key missing")
        need((PROFILES / f"{row}.json").exists(),
             f"profiles: {row}.json missing (run cc-fleet repair)")
        need(f"keyget {row}-upstream" in proxy_sh, f"start-proxy.sh: no `keyget {row}-upstream`")
        need(f"{seat['env_key']}=" in proxy_sh, f"start-proxy.sh: no export of {seat['env_key']}")
        need(f"os.environ/{seat['env_key']}" in litellm,
             f"config.yaml: no os.environ/{seat['env_key']}")
        need(f"{row})" in statusline, f"statusline.sh: no `{row})` case arm")
        need(seat["account"] in statusline, f"statusline.sh: display name {seat['account']!r} missing")
        need(f'"{row}"' in identity, f"seat-identity.py: seat {row!r} not mentioned")
        if seat.get("role") == "executor":
            need(f'"{row}"' in TIER_GUARD_CCFLEET.read_text(),
                 f"cc-fleet-tier-guard.py: executor seat {row!r} not in EXECUTOR_PROVIDERS")

    need(f"cc-fleet run {lead}" in ZSHRC.read_text(),
         f".zshrc: lead launcher `cc-fleet run {lead}` (k3m alias) missing")
    if TMUX_CONF.exists():
        need("cc-fleet run" not in TMUX_CONF.read_text(),
             ".tmux.conf: stale `cc-fleet run` binding (tmux launchers are retired)")

    for name in m.get("retired_names", []):
        pat = token_re(name)
        for f in MECHANISM_FILES:
            if not f.exists():
                continue
            for i, line in enumerate(f.read_text(errors="replace").splitlines(), 1):
                if pat.search(line):
                    fails.append(f"retired seat name {name!r} live at {f}:{i}")

    for msg in fails:
        print(f"FAIL {msg}")
    print(f"check: {'FAIL — ' + str(len(fails)) + ' finding(s)' if fails else 'OK'}")
    return 1 if fails else 0


# ---------------------------------------------------------------- rename

def rename(old, new, env_old=None):
    m = load_manifest()
    if new not in m["seat"]:
        die(f"{new!r} not in fleet_seats.toml [seat.*] — the manifest leads, add it first")
    if old not in m.get("retired_names", []):
        die(f"{old!r} not in retired_names — record the retirement in the manifest first")
    env_new = m["seat"][new]["env_key"]

    pairs = [
        (f"{old}-upstream.key", f"{new}-upstream.key"),
        (f"{old}-upstream", f"{new}-upstream"),
        (f"{old}.key", f"{new}.key"),
    ]
    if env_old and env_old != env_new:
        pairs.append((env_old, env_new))
    bare = token_re(old)

    for f in MECHANISM_FILES + DOC_FILES:
        if not f.exists():
            continue
        text = orig = f.read_text()
        for a, b in pairs:
            text = re.sub(rf"(?<![\w%*-]){re.escape(a)}(?![\w%*-])", b, text)
        text = bare.sub(new, text)
        if text != orig:
            f.write_text(text)
            print(f"rewrote {f}")

    for suffix in (".key", "-upstream.key"):
        src, dst = SECRETS / f"{old}{suffix}", SECRETS / f"{new}{suffix}"
        if src.exists():
            shutil.move(src, dst)
            print(f"secret {src.name} -> {dst.name}")

    for stale in (PROFILES / f"{old}.json", PROFILES / f"{old}-upstream.json"):
        if stale.exists():
            stale.unlink()
            print(f"deleted stale profile {stale.name}")

    r = subprocess.run(["cc-fleet", "repair"], capture_output=True, text=True)
    if r.returncode != 0:
        die(f"cc-fleet repair failed: {r.stderr.strip()}")
    print("cc-fleet repair ok")
    print(f"renamed {old} -> {new}. After the last seat: seat_tool check, restart "
          "the proxy (launchctl kickstart -k gui/$UID/com.manifold.litellm-proxy), "
          "canary one subagent per seat.")


# ---------------------------------------------------------------- slots

def read_slot_from_providers(slot, lead):
    text = PROVIDERS.read_text()
    mm = re.search(rf"\[{re.escape(lead)}\](.*?)(?=\n\[|\Z)", text, re.DOTALL)
    km = re.search(rf'^{SLOT_TO_TOML_KEY[slot]}\s*=\s*"([^"]+)"', mm.group(1), re.MULTILINE)
    return km.group(1) if km else None


def assign(slot, model):
    if slot == "fable":
        die("fable is the lead slot — pinned to k3 by the k3m alias (~/.zshrc), never rotated here")
    lead = load_manifest()["lead_seat"]
    profile = PROFILES / f"{lead}.json"

    text = PROVIDERS.read_text()
    key = SLOT_TO_TOML_KEY[slot]
    new, n = re.subn(
        rf'(\[{re.escape(lead)}\].*?^){key}\s*=\s*"[^"]+"',
        lambda mm: f'{mm.group(1)}{key} = "{model}"',
        text, count=1, flags=re.DOTALL | re.MULTILINE,
    )
    if n != 1:
        die(f"{key} not found under [{lead}] in {PROVIDERS}")
    PROVIDERS.write_text(new)
    print(f"providers.toml [{lead}] {key} = {model}")

    r = subprocess.run(["cc-fleet", "repair"], capture_output=True, text=True)
    if r.returncode != 0:
        die(f"cc-fleet repair failed: {r.stderr.strip()}")
    env = json.loads(profile.read_text())["env"]
    actual = env[SLOT_TO_ENV[slot]]
    if actual != model:
        die(f"profile verify failed: {SLOT_TO_ENV[slot]} = {actual!r}, expected {model!r}")
    print(f"profile verified: {SLOT_TO_ENV[slot]} = {model}")

    guard = load_naming_guard()
    _, label = guard.backend_for_slot(slot, {SLOT_TO_ENV[slot]: model})
    if model not in guard.SHORT_LABEL and not model.startswith("claude-"):
        print(f"WARNING: no SHORT_LABEL for {model!r} in {NAMING_GUARD.name}; "
              f"lanes will be named {label!r} — extend the guard's map")
    print(f"naming-guard lane label for {slot!r} = {label!r} (derived from env at spawn time)")

    if f"model_name: {model}" not in LITELLM_CONFIG.read_text():
        print(f"WARNING: {model!r} not in litellm model_list — proxy cannot serve it yet")
    tier_src = TIER_GUARD_SPAWN.read_text()
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
    lead = load_manifest()["lead_seat"]
    env = json.loads((PROFILES / f"{lead}.json").read_text())["env"]
    guard = load_naming_guard()
    served = LITELLM_CONFIG.read_text()
    print(f"lead seat: {lead}")
    print(f"{'slot':8} {'providers.toml':22} {'profile env':22} {'lane label':10} served?")
    for slot in SLOT_TO_TOML_KEY:
        toml_val = read_slot_from_providers(slot, lead) or "—"
        env_val = env.get(SLOT_TO_ENV[slot], "—")
        _, label = guard.backend_for_slot(slot, env)
        yes = "yes" if f"model_name: {toml_val}" in served else "NO"
        flag = "" if toml_val == env_val else "  <- DRIFT"
        print(f"{slot:8} {toml_val:22} {env_val:22} {label:10} {yes}{flag}")


if __name__ == "__main__":
    args = sys.argv[1:]
    if args == ["show"]:
        show()
    elif args == ["check"]:
        sys.exit(check())
    elif len(args) == 3 and args[0] == "assign" and args[1] in SLOT_TO_TOML_KEY:
        assign(args[1], args[2])
    elif args[:1] == ["rename"] and len(args) in (3, 5):
        if len(args) == 5 and args[3] != "--env-old":
            die("usage: seat_tool rename <old> <new> [--env-old KEY]")
        rename(args[1], args[2], args[4] if len(args) == 5 else None)
    else:
        die("usage: seat_tool show | check | assign <haiku|sonnet|opus> <model> | "
            "rename <old> <new> [--env-old KEY]")
