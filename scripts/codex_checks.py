#!/usr/bin/env python3
"""Read-only check selection for Codex workflow tooling."""
import argparse
import json
import subprocess
import sys
from pathlib import Path

from gate_workspace import Workspace


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
        'scripts/test_gate_readiness.py': {'scripts/gate_readiness.py', 'scripts/gate_workspace.py',
            'scripts/gate_policy.py', 'scripts/cpu_scope.py', 'scripts/landing_gate.py',
            'scripts/test_gate_readiness.py', 'scripts/fixtures/gate-p1.json', '.config/nextest.toml'},
        "scripts/test_agent_worktree.py": {"scripts/agent-worktree.py", "scripts/codex_brokers.py", "scripts/test_agent_worktree.py"},
        "scripts/test_rt_noise_gate.py": {"scripts/rt_noise_gate.py", "scripts/test_rt_noise_gate.py", "scripts/rt_noise_baseline.json", "scripts/trunk_health.py"},
        "scripts/test_codex_checks.py": {"scripts/codex_checks.py", "scripts/test_codex_checks.py", "scripts/landing_gate.py", "scripts/run_ui_flows.py", "scripts/gpu_proofs_gate.py", "scripts/gpu_scope.py", "scripts/ui-flows/manifest.json"},
        "scripts/test_landing_gate.py": {"scripts/landing_gate.py", "scripts/gpu_scope.py", "scripts/cpu_scope.py", "scripts/diff_scope.py", "scripts/land_branch.py", "scripts/test_landing_gate.py", "scripts/trunk_health.py"},
        "scripts/test_run_ui_flows.py": {"scripts/run_ui_flows.py", "scripts/test_run_ui_flows.py", "scripts/gpu_queue.py"},
        "scripts/test_ui_flows_batch_proof.py": {"scripts/ui_flows_batch_proof.py", "scripts/test_ui_flows_batch_proof.py", "scripts/run_ui_flows.py", "scripts/test_run_ui_flows.py"},
        "scripts/test_gpu_proofs_gate.py": {"scripts/gpu_proofs_gate.py", "scripts/test_gpu_proofs_gate.py", "scripts/gpu_scope.py"},
        "scripts/test_gate_passes.py": {"scripts/gate_passes.py", "scripts/test_gate_passes.py", "scripts/landing_gate.py", "scripts/gpu_proofs_gate.py", "scripts/gpu_queue.py"},
        "scripts/test_gpu_scope.py": {"scripts/gpu_scope.py", "scripts/test_gpu_scope.py"},
        "scripts/test_codex_prepare.py": {"scripts/codex_prepare.py", "scripts/codex_subsystems.json", "scripts/test_codex_prepare.py", "scripts/codex_checks.py"},
        "scripts/test_codex_usage.py": {"scripts/codex_usage.py", "scripts/test_codex_usage.py"},
        "scripts/test_storage_budget.py": {"scripts/storage_budget.py", "scripts/test_storage_budget.py"},
        "scripts/test_landing_gate_storage.py": {"scripts/landing_gate.py", "scripts/test_landing_gate_storage.py", "scripts/storage_budget.py"},
        "scripts/test_codex_regressions.py": {"scripts/codex_regressions.py", "scripts/codex_regressions.json", "scripts/test_codex_regressions.py", "scripts/codex_checks.py", "scripts/ui-flows/manifest.json"},
        ".codex/hooks/test_guard.py": {".codex/hooks/guard.py", ".codex/hooks/test_guard.py", ".codex/hooks.json", "scripts/storage_budget.py"},
        ".codex/hooks/test_context.py": {".codex/hooks/guard.py", ".codex/hooks/context.py", ".codex/hooks/test_context.py", "scripts/codex_prepare.py", "scripts/codex_subsystems.json", ".codex/hooks.json"},
        # The tool inventory covers every script: any script added, renamed or
        # removed must keep scripts/dev.py and scripts/TOOLS.md in step.
        "scripts/test_landing_metrics.py": {"scripts/landing_metrics.py", "scripts/test_landing_metrics.py"},
        "scripts/test_dev.py": {"scripts/dev.py", "scripts/TOOLS.md", "scripts/test_dev.py"} | {
            p for p in paths if p.startswith("scripts/") and p.endswith((".py", ".sh"))},
    }
    for name in ('watch_land', 'trunk_health', 'fleet_health'):
        tooling[f'scripts/test_{name}.py'] = {f'scripts/{name}.py', f'scripts/test_{name}.py'}
    tooling['scripts/test_feature_matrix.py'] = {'scripts/feature_matrix.py',
                                                'scripts/test_feature_matrix.py', 'scripts/gpu_queue.py'}
    for test in ('scripts/test_landing_gate.py', 'scripts/test_gpu_proofs_gate.py'):
        tooling[test].add('scripts/gate_cancellation.py')
    shared = {'scripts/gate_workspace.py', 'scripts/gate_policy.py'}
    tooling['scripts/test_codex_checks.py'].update(shared | {'scripts/cpu_scope.py'})
    for test in ('scripts/test_gpu_scope.py', 'scripts/test_gate_passes.py',
                 'scripts/test_gpu_proofs_gate.py', 'scripts/test_landing_gate.py'):
        tooling[test].update(shared)
    tooling['scripts/test_gpu_queue.py'] = {'scripts/test_gpu_queue.py', 'scripts/gpu_queue.py',
                                          'scripts/trunk_health.py'}
    # Reference checks also run when a covered regression source changes.
    from codex_regressions import inventory
    for item in inventory(repo):
        tooling["scripts/test_codex_regressions.py"].add(str(Path(item["source"]).relative_to(repo)))
    checks = [{"name": test, "argv": ["python3", "-B", str(repo / test)], "cwd": str(repo)}
              for test, triggers in tooling.items() if set(paths) & triggers]
    if any(p in ("Cargo.toml", "scripts/feature_matrix.py") or
           (p.startswith("crates/") and p.endswith("/Cargo.toml")) for p in paths):
        checks.append({"name": "feature-coverage",
                       "argv": ["python3", "-B", str(repo / "scripts/feature_matrix.py"), "--check-coverage"],
                       "cwd": str(repo)})
    return checks


def build_plan(repo: Path, paths=None):
    repo = Path(repo).resolve()
    paths = changed_paths(repo, "origin/main") if paths is None else sorted(set(paths))
    if any(Path(p).is_absolute() or not (repo / p).resolve().is_relative_to(repo) for p in paths):
        raise RuntimeError("explicit paths must stay within --repo")
    paths = sorted({(repo / p).resolve().relative_to(repo).as_posix() for p in paths})
    import cpu_scope
    import gpu_scope
    workspace = Workspace(repo)
    cpu_plan = cpu_scope.plan_for_paths(paths, repo, workspace=workspace)
    packages = sorted({workspace.owner(path) for path in paths} - {None})
    scope = gpu_scope.plan_for_paths(paths, repo, workspace=workspace, cpu_plan=cpu_plan)
    checks = tooling_checks(repo, paths)
    def check(name, argv):
        if 'cargo' in argv or name in {'ui-flows', 'gpu-proofs'}:
            argv = ['env', 'CARGO_BUILD_JOBS=4', *argv]
        checks.append({"name": name, "argv": argv, "cwd": str(repo)})
    flows = flow_filters_for_paths(repo, paths)
    if packages:
        manifest = str(repo / "Cargo.toml")
        args = [x for p in packages for x in ("-p", p)]
        check("clippy", ["cargo", "clippy", "--manifest-path", manifest, *args, "--tests", "--", "-D", "warnings"])
    for package, filterset in cpu_plan.selections().items():
        if package in cpu_plan.whole or not filterset or filterset == "none()":
            continue
        common = ["--manifest-path", str(repo / "Cargo.toml"), "-p", package, "-E", filterset]
        check(f"tests-build/{package}", ["cargo", "nextest", "run", "--no-run", *common])
        check(f"tests/{package}", ["python3", str(repo / "scripts/gpu_queue.py"), "--",
                                   "cargo", "nextest", "run", "--no-fail-fast", *common])
    if flows:
        check("ui-flows", ["python3", str(repo / "scripts/run_ui_flows.py"), *flows])
    if scope.active:
        argv = ["python3", str(repo / "scripts/gpu_proofs_gate.py"), "--manifest-path", str(repo / "Cargo.toml")]
        for p in paths:
            argv += ["--path", p]
        check("gpu-proofs", argv)
    warnings = []
    whole = sorted(cpu_plan.whole)
    if whole:
        warnings.append("whole-package CPU selections (" + ", ".join(whole) + ") omit manual worker tests; lead must choose focused validation and the landing gate handles broad scope")
    if packages or any(p in ("Cargo.toml", "Cargo.lock") for p in paths):
        warnings.append("direct reverse-dependency expansion and mandatory landing checks remain landing-gate responsibility; review broader impact explicitly")
    from codex_regressions import inventory
    return {"paths": paths, "packages": packages, "flow_filters": flows,
            "gpu_scope": {"filters": scope.final_filters(), "skips": scope.final_skips(), "glb": scope.glb, "unmapped": scope.unmapped}, "checks": checks, "warnings": warnings,
            "regressions": inventory(repo, paths)}


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
