#!/usr/bin/env python3
"""fleet_spawn.py — background cc-fleet subagent spawn with a liveness exit code.

The failure this exists for (2026-08-02, Peter: "exit codes are required"):
`cc-fleet subagent --background --json` exits 0 with {"ok": true, "status":
"running"} even when the provider key is dead — the job then fails minutes
later (KEY_INVALID 401) and only a manual poll ever sees it. An overnight
orchestration chain built on that spawn silently produces nothing.

Contract: spawn, then poll subagent-status through a grace window (default
90s — long enough for CLI startup + the first API call, where auth/routing
failures surface). If the job FAILS inside the window, exit 1 with the
provider's error_code/error_msg/suggestion on stderr. If it survives, exit 0
and print the job envelope (job_id et al.) on stdout — the caller's handle
for later polls. A job that fails after the grace window is a normal lane
failure, caught by the lane-health-check cron, not this script.

Enforcement: cc-fleet-tier-guard.py denies raw `cc-fleet subagent|spawn|run`
calls carrying --background in favor of this wrapper.

Usage:
  scripts/fleet_spawn.py <provider> --model <id> --timeout 90m
      --max-budget-usd 5 --prompt '<brief>' [--grace 90]

Obsolete when: cc-fleet itself fails fast on dead provider auth at spawn
(the probe moves into the binary) — then this wrapper is a pass-through.
"""
import argparse
import json
import subprocess
import sys
import time

POLL_INTERVAL_S = 10


def run_json(cmd: list[str]) -> tuple[int, dict]:
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    try:
        return proc.returncode, json.loads(proc.stdout)
    except ValueError:
        return proc.returncode, {"ok": False, "error_msg": proc.stdout + proc.stderr}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("provider")
    ap.add_argument("--model", required=True)
    ap.add_argument("--timeout", default="90m")
    ap.add_argument("--max-budget-usd", type=float, default=5.0)
    ap.add_argument("--prompt", required=True)
    ap.add_argument("--prompt-file")
    ap.add_argument("--grace", type=int, default=90,
                    help="liveness grace window in seconds")
    args = ap.parse_args()

    prompt = args.prompt
    if args.prompt_file:
        with open(args.prompt_file, encoding="utf-8") as f:
            prompt = f.read()

    rc, env = run_json([
        "cc-fleet", "subagent", args.provider,
        "--model", args.model,
        "--background",
        "--timeout", args.timeout,
        "--max-budget-usd", str(args.max_budget_usd),
        "--json", "--prompt", prompt,
    ])
    if rc != 0 or not env.get("ok"):
        print(f"fleet_spawn: spawn call itself failed (exit {rc}): "
              f"{env.get('error_msg') or env}", file=sys.stderr)
        return 1

    job_id = env.get("job_id")
    if not job_id:
        print(f"fleet_spawn: spawn envelope carries no job_id: {env}",
              file=sys.stderr)
        return 1

    deadline = time.monotonic() + args.grace
    while True:
        time.sleep(POLL_INTERVAL_S)
        rc, st = run_json(["cc-fleet", "subagent-status", job_id, "--json"])
        status = st.get("status")
        if status and status != "running":
            if status in ("completed", "succeeded", "success"):
                print(json.dumps(st))
                return 0
            print(f"fleet_spawn: job died inside the liveness window: "
                  f"status={status} error_code={st.get('error_code')} "
                  f"error_msg={st.get('error_msg')} "
                  f"suggestion={st.get('suggestion')}", file=sys.stderr)
            return 1
        if time.monotonic() >= deadline:
            # Survived the grace window: alive as far as spawn-time
            # liveness can tell. Hand the handle back.
            print(json.dumps(env))
            return 0


if __name__ == "__main__":
    sys.exit(main())
