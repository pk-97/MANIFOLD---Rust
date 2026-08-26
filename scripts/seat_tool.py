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
  seat_tool onboard <model> --provider <seat> [--label L] [--slot S]
                    [--costs IN OUT CACHE]
                                     add a model to an existing provider end to end
  seat_tool offboard <model>         remove a model from proxy + guards

onboard does the whole add-a-model chain: copies the provider's nearest
sibling config.yaml entry (api_base, key env, rates unless --costs), copies
its 400-retry policy when present, extends every virtual key that already
reaches a sibling model, restarts the proxy, fires a live verification call,
and patches the guard maps (SHORT_LABEL, EXECUTOR_TIERS). --slot also runs
assign. offboard reverses all of it (refuses while a slot still points at
the model — assign away first). Neither touches a running session's env.

rename rewrites seat tokens in all registered mechanism files (home configs +
repo hooks + the two fleet docs), renames the secret files, deletes stale
profiles, and runs `cc-fleet repair`. Token matching is word-boundary strict:
`glm` the seat never matches `glm-4.7` the model, `%glm%` SQL patterns, or
`glm*` shell globs. After renaming every seat: run `check`, restart the proxy
(env var names move with --env-old), canary one subagent per seat. check is
the drift gate — hand edits or upgrades that resurrect a retired name fail
loudly here.
"""
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.request
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
LAUNCH_GUARD = HOOKS / "agent-launch-guard.py"
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


def load_launch_guard():
    import importlib.util
    spec = importlib.util.spec_from_file_location("launch_guard", LAUNCH_GUARD)
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

    # No per-role assertions against cc-fleet-tier-guard.py: since 4d0d6baa6
    # (2026-08-02) that guard denies every spawn verb for every tier and names
    # no provider seats, so the old EXECUTOR_PROVIDERS check could never pass.

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

    guard = load_launch_guard()
    _, label = guard.backend_for_slot(slot, {SLOT_TO_ENV[slot]: model})
    if model not in guard.SHORT_LABEL and not model.startswith("claude-"):
        print(f"WARNING: no SHORT_LABEL for {model!r} in {LAUNCH_GUARD.name}; "
              f"lanes will be named {label!r} — extend the guard's map")
    print(f"launch-guard lane label for {slot!r} = {label!r} (derived from env at spawn time)")

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
    guard = load_launch_guard()
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


# ---------------------------------------------------------------- onboard / offboard

PROXY = "http://127.0.0.1:4000"
MASTER_KEY_FILE = HOME / ".config/litellm/master_key"
PROXY_SERVICE = "com.manifold.litellm-proxy"


def proxy_api(path, payload=None):
    """Admin-API call against the local proxy (master-key auth)."""
    req = urllib.request.Request(
        PROXY + path,
        data=json.dumps(payload).encode() if payload is not None else None,
        method="POST" if payload is not None else "GET",
        headers={"Authorization": "Bearer sk-" + MASTER_KEY_FILE.read_text().strip(),
                 "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read())


def model_blocks(text):
    """(start, end, name) spans of every `  - model_name:` entry in config.yaml."""
    starts = [(m.start(), m.group(1))
              for m in re.finditer(r"^  - model_name: (\S+)\s*$", text, re.MULTILINE)]
    return [(s, starts[i + 1][0] if i + 1 < len(starts) else len(text), name)
            for i, (s, name) in enumerate(starts)]


def provider_models(text, env_key):
    """Names of every model_list entry keyed by the provider's env var."""
    return {name for s, e, name in model_blocks(text)
            if f"os.environ/{env_key}" in text[s:e]}


def restart_proxy():
    subprocess.run(["launchctl", "kickstart", "-k",
                    f"gui/{os.getuid()}/{PROXY_SERVICE}"], check=True)
    for _ in range(45):
        try:
            proxy_api("/health/liveliness")
            print("proxy restarted, live")
            return
        except Exception:
            time.sleep(2)
    die("proxy did not come back within 90s — check ~/.config/litellm/proxy.log")


def each_key():
    """Yield (token, alias, models) for every virtual key."""
    for token in proxy_api("/key/list")["keys"]:
        info = proxy_api(f"/key/info?key={token}")["info"]
        yield token, info.get("key_alias"), info.get("models") or []


def onboard(model, provider, label=None, slot=None, costs=None):
    m = load_manifest()
    if provider not in m["seat"]:
        die(f"{provider!r} not in fleet_seats.toml [seat.*] — onboard adds a model "
            "to an EXISTING provider; a new provider is the manual procedure in "
            "docs/PROVIDER_OPERATIONS.md")
    env_key = m["seat"][provider]["env_key"]

    text = LITELLM_CONFIG.read_text()
    blocks = model_blocks(text)
    if any(name == model for _, _, name in blocks):
        die(f"{model!r} already in litellm model_list")
    sibs = [(s, e, name) for s, e, name in blocks
            if f"os.environ/{env_key}" in text[s:e]]
    if not sibs:
        die(f"no config.yaml entry uses os.environ/{env_key} — provider wiring missing")
    s, e, sib = sibs[-1]

    new_block, n1 = re.subn(r"^(  - model_name: )\S+", rf"\g<1>{model}",
                            text[s:e], count=1, flags=re.MULTILINE)
    new_block, n2 = re.subn(r"^(      model: \S+/)\S+\s*$", rf"\g<1>{model}",
                            new_block, count=1, flags=re.MULTILINE)
    if n1 != 1 or n2 != 1:
        die(f"sibling block for {sib!r} has an unexpected shape — onboard by hand")
    if costs:
        for key, val in zip(("input_cost_per_token", "output_cost_per_token",
                             "cache_read_input_token_cost"), costs):
            new_block, n = re.subn(rf"^(      {key}: )\S+", rf"\g<1>{val}",
                                   new_block, count=1, flags=re.MULTILINE)
            if n != 1:
                die(f"sibling block has no {key} line — set pricing by hand")
    else:
        print(f"NOTE: rates copied from {sib!r} — verify pricing for {model!r}")
    if "model_info:" in new_block:
        print(f"NOTE: model_info copied from {sib!r} — verify token limits for {model!r}")
    comment = f"  # {model} — onboarded {datetime.date.today()} via seat_tool onboard.\n"
    text = text[:e] + comment + new_block + text[e:]

    pol = None
    for name in provider_models(text, env_key):
        pol = re.search(rf"^    {re.escape(name)}:\n(?:      .*\n)+", text, re.MULTILINE)
        if pol:
            break
    if pol:
        text = (text[:pol.end()] + f"    {model}:\n      BadRequestErrorRetries: 3\n"
                + text[pol.end():])
        print("retry policy: copied BadRequestErrorRetries from provider sibling")
    LITELLM_CONFIG.write_text(text)
    print(f"config.yaml: model_list += {model}")

    sib_models = provider_models(text, env_key) - {model}
    for token, alias, models in each_key():
        if set(models) & sib_models and model not in models:
            proxy_api("/key/update", {"key": token, "models": models + [model]})
            print(f"key {alias}: + {model}")

    restart_proxy()
    resp = proxy_api("/v1/chat/completions", {
        "model": model,
        "messages": [{"role": "user", "content": "Reply with exactly: PONG"}],
        "max_tokens": 512})
    content = resp["choices"][0]["message"].get("content") or ""
    print(f"verification call: served, content {content[:40]!r}")
    if not content.strip():
        print("WARNING: empty content — the model likely reasons unconditionally; "
              "small max_tokens budgets return nothing")

    guard_src = LAUNCH_GUARD.read_text()
    if f'"{model}"' not in guard_src:
        lbl = label or re.sub(r"[^a-z0-9]", "", model.split("-")[-1].lower()) or model
        if not label:
            print(f"NOTE: lane label derived as {lbl!r} — pass --label to override")
        guard_src, n = re.subn(r"(SHORT_LABEL = \{\n)",
                               f'\\g<1>    "{model}": "{lbl}",\n', guard_src, count=1)
        if n != 1:
            die("SHORT_LABEL insert failed")
        LAUNCH_GUARD.write_text(guard_src)
        print(f"{LAUNCH_GUARD.name}: SHORT_LABEL[{model!r}] = {lbl!r} — commit it")

    tier_src = TIER_GUARD_SPAWN.read_text()
    if model not in tier_src:
        tier_src, n = re.subn(r'(EXECUTOR_TIERS\s*=\s*re\.compile\(\s*r"[^"]+)',
                              rf"\g<1>|{model}", tier_src, count=1)
        if n != 1:
            die("EXECUTOR_TIERS patch failed")
        TIER_GUARD_SPAWN.write_text(tier_src)
        print(f"{TIER_GUARD_SPAWN.name}: EXECUTOR_TIERS += {model} — commit it")

    if slot:
        assign(slot, model)
    else:
        print("done. Assign a slot with: seat_tool assign <haiku|sonnet|opus> " + model)


def offboard(model):
    lead = load_manifest()["lead_seat"]
    for slot in SLOT_TO_TOML_KEY:
        if read_slot_from_providers(slot, lead) == model:
            die(f"{model!r} still fills the {slot} slot — assign away first")

    text = LITELLM_CONFIG.read_text()
    blocks = [(s, e) for s, e, name in model_blocks(text) if name == model]
    if not blocks:
        die(f"{model!r} not in litellm model_list")
    s, e = blocks[0]
    com = re.search(r"^  # [^\n]*seat_tool onboard[^\n]*\n\Z", text[:s], re.MULTILINE)
    if com:
        s = com.start()
    text = text[:s] + text[e:]
    text, n = re.subn(rf"^    {re.escape(model)}:\n(?:      .*\n)+", "",
                      text, flags=re.MULTILINE)
    if n:
        print("retry policy: removed")
    LITELLM_CONFIG.write_text(text)
    print(f"config.yaml: model_list -= {model}")

    for token, alias, models in each_key():
        if model in models:
            proxy_api("/key/update",
                      {"key": token, "models": [x for x in models if x != model]})
            print(f"key {alias}: - {model}")

    restart_proxy()

    guard_src = LAUNCH_GUARD.read_text()
    guard_src, n = re.subn(rf'^    "{re.escape(model)}": "[^"]+",\n', "",
                           guard_src, count=1, flags=re.MULTILINE)
    if n:
        LAUNCH_GUARD.write_text(guard_src)
        print(f"{LAUNCH_GUARD.name}: SHORT_LABEL -= {model} — commit it")
    tier_src = TIER_GUARD_SPAWN.read_text()
    tier_src, n = re.subn(rf"\|{re.escape(model)}", "", tier_src, count=1)
    if n:
        TIER_GUARD_SPAWN.write_text(tier_src)
        print(f"{TIER_GUARD_SPAWN.name}: EXECUTOR_TIERS -= {model} — commit it")
    print("done. Prose follow-ups: docs/AGENT_ROUTING.md, memory seat-proxy line.")


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
    elif args[:1] == ["onboard"] and len(args) >= 4:
        model, opts = args[1], args[2:]

        def opt(flag, n=1):
            if flag not in opts:
                return None
            vals = opts[opts.index(flag) + 1: opts.index(flag) + 1 + n]
            if len(vals) != n:
                die(f"{flag} expects {n} value(s)")
            return vals

        prov = opt("--provider")
        if not prov:
            die("onboard requires --provider <seat> (a seat from fleet_seats.toml)")
        costs = opt("--costs", 3)
        slot = opt("--slot")
        label = opt("--label")
        if slot and slot[0] not in SLOT_TO_TOML_KEY:
            die(f"--slot must be one of {', '.join(SLOT_TO_TOML_KEY)}")
        onboard(model, prov[0], label=label[0] if label else None,
                slot=slot[0] if slot else None, costs=costs)
    elif args[:1] == ["offboard"] and len(args) == 2:
        offboard(args[1])
    else:
        die("usage: seat_tool show | check | assign <haiku|sonnet|opus> <model> | "
            "rename <old> <new> [--env-old KEY] | "
            "onboard <model> --provider <seat> [--label L] [--slot S] [--costs IN OUT CACHE] | "
            "offboard <model>")
