#!/usr/bin/env python3
"""Read-only check selection for Codex workflow tooling."""
import argparse
import json
import subprocess
import sys
from pathlib import Path


def _git(repo, *args):
    r = subprocess.run(["git", *args], cwd=repo, text=True,
                       capture_output=True)
    if r.returncode:
        raise RuntimeError(r.stderr.strip() or f"git {' '.join(args)} failed")
    return r.stdout


def changed_paths(repo, base):
    """Return changed paths, including both sides of renames and worktree dirt."""
    merge = _git(repo, "merge-base", base, "HEAD").strip()
    lines = []
    for args in (("diff", "--name-only", "--no-renames", "-z", merge, "HEAD"),
                 ("diff", "--name-only", "--no-renames", "-z", "HEAD"),
                 ("ls-files", "--others", "--exclude-standard", "-z")):
        lines.extend(p for p in _git(repo, *args).split("\0") if p)
    return sorted(set(lines))


def flow_filters_for_paths(repo, paths):
    from run_ui_flows import filters_for_paths
    with open(repo / "scripts/ui-flows/manifest.json") as f:
        return filters_for_paths(paths, json.load(f))[0]


def tooling_checks(repo, paths):
    """Shared worker/landing selection; does not execute tests."""
    tooling = {
        "scripts/test_codex_checks.py": {"scripts/codex_checks.py", "scripts/test_codex_checks.py", "scripts/landing_gate.py", "scripts/run_ui_flows.py", "scripts/gpu_proofs_gate.py", "scripts/ui-flows/manifest.json"},
        "scripts/test_codex_prepare.py": {"scripts/codex_prepare.py", "scripts/codex_subsystems.json", "scripts/test_codex_prepare.py", "scripts/codex_checks.py"},
        "scripts/test_codex_usage.py": {"scripts/codex_usage.py", "scripts/test_codex_usage.py"},
        ".codex/hooks/test_guard.py": {".codex/hooks/guard.py", ".codex/hooks/test_guard.py", ".codex/hooks.json"},
        ".codex/hooks/test_context.py": {".codex/hooks/guard.py", ".codex/hooks/context.py", ".codex/hooks/test_context.py", "scripts/codex_prepare.py", "scripts/codex_subsystems.json", ".codex/hooks.json"},
    }
    return [{"name": test, "argv": ["python3", "-B", str(repo / test)], "cwd": str(repo)}
            for test, triggers in tooling.items() if set(paths) & triggers]


def build_plan(repo: Path, paths=None):
    repo = Path(repo).resolve()
    paths = changed_paths(repo, "origin/main") if paths is None else sorted(set(paths))
    if any(Path(p).is_absolute() or not (repo / p).resolve().is_relative_to(repo) for p in paths):
        raise RuntimeError("explicit paths must stay within --repo")
    paths = sorted({(repo / p).resolve().relative_to(repo).as_posix() for p in paths})
    from landing_gate import gpu_proofs_scope_for_paths, _path_is_gpu, packages_for_paths
    packages = packages_for_paths(repo, paths)
    scope = gpu_proofs_scope_for_paths(paths)
    checks = tooling_checks(repo, paths)
    def check(name, argv):
        checks.append({"name": name, "argv": argv, "cwd": str(repo)})
    flows = flow_filters_for_paths(repo, paths)
    if flows:
        check("ui-flows", ["python3", str(repo / "scripts/run_ui_flows.py"), *flows])
    if packages:
        manifest = str(repo / "Cargo.toml")
        args = [x for p in packages for x in ("-p", p)]
        checks += [{"name": "clippy", "argv": ["cargo", "clippy", "--manifest-path", manifest, *args, "--tests", "--", "-D", "warnings"], "cwd": str(repo)},
                   {"name": "tests", "argv": ["cargo", "nextest", "run", "--manifest-path", manifest, *args], "cwd": str(repo)}]
    if any(_path_is_gpu(p) for p in paths):
        argv = ["python3", str(repo / "scripts/gpu_proofs_gate.py"), "--manifest-path", str(repo / "Cargo.toml")]
        if scope:
            f, s = scope
            argv += sum((["--filter", x] for x in f), []) + sum((["--skip", x] for x in s), [])
        checks.append({"name": "gpu-proofs", "argv": argv, "cwd": str(repo)})
    warnings = []
    if packages or any(p in ("Cargo.toml", "Cargo.lock") for p in paths):
        warnings.append("direct reverse-dependency expansion and mandatory landing checks remain landing-gate responsibility; review broader impact explicitly")
    return {"paths": paths, "packages": packages, "flow_filters": flows,
            "gpu_scope": scope, "checks": checks, "warnings": warnings}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--repo", type=Path, default=Path.cwd())
    p.add_argument("--base", default="origin/main")
    p.add_argument("--path", action="append", dest="paths")
    p.add_argument("--json", action="store_true")
    a = p.parse_args()
    try:
        plan = build_plan(a.repo, a.paths if a.paths is not None else changed_paths(a.repo, a.base))
    except (RuntimeError, OSError, ValueError) as e:
        print(f"codex checks: {e}", file=sys.stderr)
        return 2
    if a.json:
        print(json.dumps(plan, indent=2, sort_keys=True))
    else:
        import shlex
        for c in plan["checks"]:
            print(f"{c['name']} (cwd {c['cwd']}): {shlex.join(c['argv'])}")
        for warning in plan["warnings"]:
            print(f"Warning: {warning}")
        if not plan["checks"]:
            print("No mapped worker checks; lead must choose validation for this scope.")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
