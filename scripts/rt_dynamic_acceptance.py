#!/usr/bin/env python3
"""Scene-modifier RT acceptance runner (SCENE_MODIFIER_RT_ACCEPTANCE.md A0/A10).

Dispatches the acceptance modes of the dynamic-RT-on-modified-meshes campaign
and writes one evidence report per run. This script is a dispatcher, not a
second test framework: every mode runs an existing cargo or gate entry point,
parses that run's own output, and records it. It never re-derives a verdict
the underlying harness did not produce.

MODES
  cpu      A1 mesh-change contract: `cargo test -p manifold-renderer mesh_change_`
  gpu      A2-A5 correctness groups via scripts/gpu_proofs_gate.py --filter,
           one gate run per group (baseline, fusion, ordering, shading,
           current_frame, refit — catalog and perf are excluded here per A0)
  catalog  A6 stock-catalog group: gpu_proofs_gate.py --filter rt_dynamic_catalog
  export   A7 production export: cargo test -p manifold-app --features
           journey-proofs rt_dynamic_export_ -- --test-threads=1
           (requires ffmpeg and ffprobe on PATH)
  perf     A9 bounded performance: explicitly invoked release-build proof,
           `cargo test --release -p manifold-renderer --features gpu-proofs
           --test gpu_proofs -- rt_dynamic_perf --test-threads=1`.
           Requires --reference-project and --held-out-project; both files'
           SHA-256 hashes are recorded in the report fixtures.

GROUPS WHOSE TESTS DO NOT EXIST YET (catalog, export, perf, and not-yet-landed
gpu groups) still have a dispatch entry. Absence is detected by listing tests
(`cargo test -- --list`), recorded as a blocked group ("tests not yet
implemented"), and makes the run exit nonzero — A0's "zero selected tests is
a failed gate". The scaffold lands before those modules exist; a blocked
report is never a passing qualification.

HOST AVAILABILITY: gpu/catalog/perf modes require a native Metal RT host.
A non-Darwin platform, or a Darwin host whose reported GPU family is known
to lack Metal raytracing (Intel), yields a blocked report and a nonzero exit
without running cargo. Detection is conservative: an unrecognized family is
recorded and the dispatched tests remain the final oracle — a run can only
pass when the tests themselves passed.

REPORT (A0): schemaVersion 1, gitSha, dirtyPaths, hardware, osVersion, mode,
fixtures[{name,hash}], tests[{name,status,observed,required,artifactPaths}],
metrics, commands, exitCode. GPU-family modes additionally record gpuFamily,
sample/resolution settings, output precision and validation-layer state.
Command logs land under /tmp/manifold-rt-dynamic/<git-sha>/<mode>-<stamp>/.

EXIT CODES
  0  every selected group passed
  1  a test failed, a group is blocked/missing, or the host cannot run the mode
  2  the runner itself could not complete (list failed, report unwritable)

USAGE
  scripts/rt_dynamic_acceptance.py --manifest-path Cargo.toml --mode cpu \
      --report /tmp/rt-acc-cpu.json
  scripts/rt_dynamic_acceptance.py --manifest-path Cargo.toml --mode perf \
      --reference-project ref.manifold --held-out-project held.manifold \
      --report /tmp/rt-acc-perf.json

Obsolete when: the acceptance tests report and aggregate their own results
into the A0 report schema, making this dispatcher redundant.
"""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# A0 group ownership: GPU mode runs the correctness groups only. catalog
# (A6) and perf (A9) are separate modes; rt_dynamic_perf is additionally
# excluded because A0 makes it an explicitly invoked release-build proof.
GPU_CORRECTNESS_GROUPS = [
    "rt_dynamic_baseline",
    "rt_dynamic_fusion",
    "rt_dynamic_ordering",
    "rt_dynamic_shading",
    "rt_dynamic_current_frame",
    "rt_dynamic_refit",
]

# Darwin GPU families known to lack Metal raytracing support. Apple Silicon
# and AMD Navi families are not listed; anything unrecognized is recorded
# and left to the tests as the oracle.
RT_INCAPABLE_GPU_MARKERS = ("intel",)

CMD_TIMEOUT_SEC = 3600

TEST_LINE_RE = re.compile(r"^\s*test (\S+) \.\.\. (ok|FAILED|ignored)\s*$")
TEST_RESULT_RE = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;"
)


def default_manifest_path() -> Path:
    return Path(__file__).resolve().parent.parent / "Cargo.toml"


def log(msg: str) -> None:
    print(msg, flush=True)


# ── environment probes (report provenance, not verdicts) ────────────────


def git_info(repo: Path) -> tuple[str, list[str]]:
    sha = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"],
        capture_output=True, text=True, timeout=30,
    ).stdout.strip()
    dirty_out = subprocess.run(
        ["git", "-C", str(repo), "status", "--porcelain"],
        capture_output=True, text=True, timeout=60,
    ).stdout
    dirty = [line[3:] for line in dirty_out.splitlines() if len(line) > 3]
    return sha, dirty


def probe_os() -> str:
    if platform.system() == "Darwin":
        ver = subprocess.run(
            ["sw_vers", "-productVersion"],
            capture_output=True, text=True, timeout=15,
        ).stdout.strip()
        return f"macOS {ver}" if ver else platform.platform()
    return platform.platform()


def probe_hardware() -> str:
    if platform.system() == "Darwin":
        model = subprocess.run(
            ["sysctl", "-n", "hw.model"],
            capture_output=True, text=True, timeout=15,
        ).stdout.strip()
        return model or "unknown-mac"
    return platform.machine() or "unknown"


def probe_gpu_family() -> str | None:
    """Best-effort GPU name for the report; None when the probe cannot run."""
    if platform.system() != "Darwin":
        return None
    try:
        proc = subprocess.run(
            ["system_profiler", "SPDisplaysDataType", "-json"],
            capture_output=True, text=True, timeout=60,
        )
        data = json.loads(proc.stdout)
        gpus = [
            item for item in data.get("SPDisplaysDataType", [])
            if item.get("sppci_device_type") == "spdisplays_gpu"
        ]
        names = [g.get("sppci_model") or g.get("_name") for g in gpus]
        names = [n for n in names if n]
        return ", ".join(names) if names else None
    except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError):
        return None


def validation_layers_enabled() -> bool:
    # Metal API validation is opted into via METAL_DEVICE_WRAPPER_TYPE=1
    # (or the Xcode scheme equivalent); A9 disables it only for timing runs.
    return os.environ.get("METAL_DEVICE_WRAPPER_TYPE") == "1"


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ── command execution ───────────────────────────────────────────────────


def run_streamed(cmd: list[str], cwd: Path, log_path: Path) -> tuple[int, float]:
    """Stream a command's merged output to the console and a log file.

    Returns (exit code, duration seconds). A timeout or spawn failure raises —
    the caller converts that into a runner-level exit 2."""
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log(f"$ {' '.join(map(str, cmd))}")
    start = time.time()
    with open(log_path, "w") as logf:
        proc = subprocess.Popen(
            cmd,
            cwd=str(cwd),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        assert proc.stdout is not None
        for line in proc.stdout:
            print(line, end="", flush=True)
            logf.write(line)
        exit_code = proc.wait(timeout=CMD_TIMEOUT_SEC)
    return exit_code, time.time() - start


def list_tests(cmd: list[str], cwd: Path, log_path: Path) -> tuple[set[str], int, float]:
    """`cargo test ... -- --list` → set of test names.

    Returns (names, exit code, duration). An empty set with exit 0 means the
    binary built and genuinely selected nothing."""
    exit_code, duration = run_streamed(cmd, cwd, log_path)
    names: set[str] = set()
    if exit_code == 0 and log_path.exists():
        for line in log_path.read_text(errors="replace").splitlines():
            if line.endswith(": test"):
                names.add(line[: -len(": test")].strip())
    return names, exit_code, duration


def parse_test_results(output: str) -> list[dict]:
    """Per-test lines cargo prints ("test <path> ... ok|FAILED") → records."""
    tests = []
    for line in output.splitlines():
        m = TEST_LINE_RE.match(line)
        if m:
            name, outcome = m.group(1), m.group(2)
            if outcome == "ignored":
                continue  # A0 forbids muted gates; an ignored acceptance
                # test is a fail, never a quiet pass.
            tests.append({
                "name": name,
                "status": "pass" if outcome == "ok" else "fail",
                "observed": outcome,
                "required": None,
                "artifactPaths": [],
            })
    return tests


def parse_result_summary(output: str) -> dict:
    passed = failed = ignored = 0
    for line in output.splitlines():
        m = TEST_RESULT_RE.match(line)
        if m:
            passed += int(m.group(2))
            failed += int(m.group(3))
            ignored += int(m.group(4))
    return {"passed": passed, "failed": failed, "ignored": ignored}


def blocked_entry(name: str, observed: str, required: str) -> dict:
    return {
        "name": name,
        "status": "blocked",
        "observed": observed,
        "required": required,
        "artifactPaths": [],
    }


# ── host availability ───────────────────────────────────────────────────


def host_gpu_available() -> tuple[bool, str]:
    """(available, reason). Conservative: only known-bad hosts block here."""
    if platform.system() != "Darwin":
        return False, (
            f"native Metal RT unavailable on {platform.system()} "
            "(Vulkan backend approved but not built, docs/VULKAN_BACKEND_DESIGN.md)"
        )
    family = probe_gpu_family()
    if family and any(m in family.lower() for m in RT_INCAPABLE_GPU_MARKERS):
        return False, f"GPU family '{family}' has no Metal raytracing support"
    return True, ""


# ── report ──────────────────────────────────────────────────────────────


def write_report(path: Path, report: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, indent=2) + "\n")
    # The report must round-trip; a malformed report is itself an A0 failure.
    json.loads(path.read_text())
    log(f"report → {path}")


def gpu_report_section(gpu_family: str | None) -> dict:
    # Required settings per A9: RtQualityColumn::default() realtime values
    # (manifold-foundation/src/settings.rs) at 1280x720. Observed values stay
    # null until the perf harness reports what it actually ran with.
    return {
        "gpuFamily": gpu_family,
        "settings": {
            "source": "SCENE_MODIFIER_RT_ACCEPTANCE.md A9 (RtQualityColumn::default())",
            "resolution": "1280x720",
            "shadows": "UltraLow (1 sample)",
            "ao": "Medium (4 samples)",
            "gi": "Medium (4 samples)",
            "reflections": "High (8 samples)",
            "rayResolution": "Half",
            "spatialDenoise": "Medium",
            "observed": None,
        },
        "outputPrecision": {"required": "half", "observed": None},
        "validationLayers": validation_layers_enabled(),
    }


# ── mode implementations ────────────────────────────────────────────────
#
# Each returns (exit_code, tests, metrics, commands, fixtures). Dispatch is
# table-driven at the bottom; a mode whose tests are absent still has its
# entry here and reports blocked rather than failing to parse.


def mode_cpu(repo: Path, manifest: Path, artifact_dir: Path):
    tests, commands = [], []
    fixtures = []
    list_cmd = ["cargo", "test", "-p", "manifold-renderer", "mesh_change_", "--", "--list"]
    list_log = artifact_dir / "list-cpu.log"
    listed, rc, dur = list_tests(list_cmd, repo, list_log)
    commands.append({"cmd": " ".join(list_cmd), "exitCode": rc,
                     "durationSec": round(dur, 1), "log": str(list_log)})
    matched = sorted(t for t in listed if "mesh_change_" in t)
    if rc != 0:
        return 2, [blocked_entry("cpu: mesh_change_", f"listing exited {rc}",
                                 "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures
    if not matched:
        tests.append(blocked_entry(
            "cpu: mesh_change_", "0 tests listed for 'mesh_change_'",
            "tests not yet implemented"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    run_log = artifact_dir / "run-cpu.log"
    run_cmd = ["cargo", "test", "-p", "manifold-renderer", "mesh_change_"]
    rc, dur = run_streamed(run_cmd, repo, run_log)
    commands.append({"cmd": " ".join(run_cmd), "exitCode": rc,
                     "durationSec": round(dur, 1), "log": str(run_log)})
    output = run_log.read_text(errors="replace") if run_log.exists() else ""
    tests = parse_test_results(output)
    for t in tests:
        t["artifactPaths"] = [str(run_log)]
    metrics = parse_result_summary(output)
    metrics["blocked"] = 0
    # Per-test parse can miss (harness output shape changed); the cargo exit
    # code is still authoritative, but a pass with no recorded tests is not
    # evidence — record the gap instead of passing silently.
    if rc == 0 and not tests:
        tests.append(blocked_entry(
            "cpu: mesh_change_", "run passed but no per-test results parsed",
            "parseable test output"))
        return 1, tests, metrics, commands, fixtures
    return (0 if rc == 0 else 1), tests, metrics, commands, fixtures


def _list_gpu_tests(repo: Path, artifact_dir: Path,
                    release: bool = False) -> tuple[set[str] | None, int, dict]:
    """List the gpu_proofs test names. Returns (names, exit, command record);
    names is None when the listing itself failed."""
    list_cmd = ["cargo", "test"]
    if release:
        list_cmd.append("--release")
    list_cmd += ["-p", "manifold-renderer", "--features", "gpu-proofs",
                 "--test", "gpu_proofs", "--", "--list"]
    list_log = artifact_dir / "list.log"
    listed, rc, dur = list_tests(list_cmd, repo, list_log)
    record = {"cmd": " ".join(list_cmd), "exitCode": rc,
              "durationSec": round(dur, 1), "log": str(list_log)}
    if rc != 0:
        return None, rc, record
    return listed, rc, record


def _mode_gpu_groups(repo: Path, manifest: Path, artifact_dir: Path,
                     groups: list[str]):
    """Shared dispatch for gpu/catalog group runs through gpu_proofs_gate.py,
    one gate run per group. Perf dispatches its release build itself instead
    (A9: one bounded run, never a debug-profile measurement)."""
    tests, commands = [], []
    listed, rc, record = _list_gpu_tests(repo, artifact_dir)
    commands.append(record)
    if listed is None:
        return 2, [blocked_entry(f"gpu: {', '.join(groups)}",
                                 f"listing exited {rc}", "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands

    metrics = {"passed": 0, "failed": 0, "blocked": 0}
    exit_code = 0
    gate = Path(__file__).resolve().parent / "gpu_proofs_gate.py"
    for group in groups:
        matched = sorted(t for t in listed if group in t)
        if not matched:
            tests.append(blocked_entry(
                f"{group}", f"0 tests listed for '{group}'",
                "tests not yet implemented"))
            metrics["blocked"] += 1
            exit_code = 1
            continue
        run_log = artifact_dir / f"run-{group}.log"
        run_cmd = ["python3", str(gate), "--manifest-path", str(manifest),
                   "--filter", group]
        rc, dur = run_streamed(run_cmd, repo, run_log)
        commands.append({"cmd": " ".join(run_cmd), "exitCode": rc,
                         "durationSec": round(dur, 1), "log": str(run_log)})
        output = run_log.read_text(errors="replace") if run_log.exists() else ""
        group_tests = parse_test_results(output)
        for t in group_tests:
            t["artifactPaths"] = [str(run_log)]
        if not group_tests:
            # Gate ran but per-test lines didn't parse: record the group as
            # one entry driven by the gate's exit code, never a silent pass.
            group_tests = [{
                "name": f"{group} (group)", "status": "pass" if rc == 0 else "fail",
                "observed": f"gate exit {rc}", "required": None,
                "artifactPaths": [str(run_log)],
            }]
        summary = parse_result_summary(output)
        metrics["passed"] += summary["passed"]
        metrics["failed"] += summary["failed"]
        if rc != 0:
            exit_code = 1
        tests.extend(group_tests)
    return exit_code, tests, metrics, commands


def mode_gpu(repo: Path, manifest: Path, artifact_dir: Path):
    exit_code, tests, metrics, commands = _mode_gpu_groups(
        repo, manifest, artifact_dir, GPU_CORRECTNESS_GROUPS)
    return exit_code, tests, metrics, commands, []


def mode_catalog(repo: Path, manifest: Path, artifact_dir: Path):
    exit_code, tests, metrics, commands = _mode_gpu_groups(
        repo, manifest, artifact_dir, ["rt_dynamic_catalog"])
    return exit_code, tests, metrics, commands, []


def mode_perf(repo: Path, manifest: Path, artifact_dir: Path,
              reference_project: Path | None, held_out_project: Path | None):
    fixtures = []
    tests = []
    missing_inputs = []
    for label, path in (("reference-project", reference_project),
                        ("held-out-project", held_out_project)):
        if path is None:
            missing_inputs.append(f"{label} not given")
        elif not Path(path).exists():
            missing_inputs.append(f"{label} not found: {path}")
        else:
            fixtures.append({"name": Path(path).name, "hash": sha256_file(Path(path))})
    if missing_inputs:
        # A9: no invented proxy pass — a missing held-out fixture blocks.
        tests.append(blocked_entry(
            "perf: inputs", "; ".join(missing_inputs),
            "--reference-project and --held-out-project"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, [], fixtures

    listed, rc, record = _list_gpu_tests(repo, artifact_dir)
    commands.append(record)
    if listed is None:
        return 2, [blocked_entry("perf: rt_dynamic_perf", f"listing exited {rc}",
                                 "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    matched = sorted(t for t in listed if "rt_dynamic_perf" in t)
    if not matched:
        tests.append(blocked_entry(
            "rt_dynamic_perf", "0 tests listed for 'rt_dynamic_perf'",
            "tests not yet implemented"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    run_log = artifact_dir / "run-rt_dynamic_perf-release.log"
    run_cmd = ["cargo", "test", "--release", "-p", "manifold-renderer",
               "--features", "gpu-proofs", "--test", "gpu_proofs", "--",
               "rt_dynamic_perf", "--test-threads=1"]
    rc, dur = run_streamed(run_cmd, repo, run_log)
    commands.append({"cmd": " ".join(run_cmd), "exitCode": rc,
                     "durationSec": round(dur, 1), "log": str(run_log)})
    output = run_log.read_text(errors="replace") if run_log.exists() else ""
    tests = parse_test_results(output)
    for t in tests:
        t["artifactPaths"] = [str(run_log)]
    metrics = parse_result_summary(output)
    metrics["blocked"] = 0
    if rc == 0 and not tests:
        tests.append(blocked_entry(
            "perf: rt_dynamic_perf", "run passed but no per-test results parsed",
            "parseable test output"))
        return 1, tests, metrics, commands, fixtures
    return (0 if rc == 0 else 1), tests, metrics, commands, fixtures


def mode_export(repo: Path, manifest: Path, artifact_dir: Path):
    tests, commands = [], []
    fixtures = []
    missing_tools = [t for t in ("ffmpeg", "ffprobe") if shutil.which(t) is None]
    if missing_tools:
        tests.append(blocked_entry(
            "export: toolchain", f"missing required tool(s): {', '.join(missing_tools)}",
            "ffmpeg and ffprobe on PATH"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    base = ["cargo", "test", "-p", "manifold-app", "--features", "journey-proofs",
            "rt_dynamic_export_"]
    list_log = artifact_dir / "list-export.log"
    listed, rc, dur = list_tests(base + ["--", "--list"], repo, list_log)
    commands.append({"cmd": " ".join(base + ["--", "--list"]), "exitCode": rc,
                     "durationSec": round(dur, 1), "log": str(list_log)})
    if rc != 0:
        return 2, [blocked_entry("export: rt_dynamic_export_", f"listing exited {rc}",
                                 "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures
    matched = sorted(t for t in listed if "rt_dynamic_export_" in t)
    if not matched:
        tests.append(blocked_entry(
            "rt_dynamic_export_", "0 tests listed for 'rt_dynamic_export_'",
            "tests not yet implemented"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    run_log = artifact_dir / "run-export.log"
    run_cmd = base + ["--", "--test-threads=1"]
    rc, dur = run_streamed(run_cmd, repo, run_log)
    commands.append({"cmd": " ".join(run_cmd), "exitCode": rc,
                     "durationSec": round(dur, 1), "log": str(run_log)})
    output = run_log.read_text(errors="replace") if run_log.exists() else ""
    tests = parse_test_results(output)
    for t in tests:
        t["artifactPaths"] = [str(run_log)]
    metrics = parse_result_summary(output)
    metrics["blocked"] = 0
    if rc == 0 and not tests:
        tests.append(blocked_entry(
            "export: rt_dynamic_export_", "run passed but no per-test results parsed",
            "parseable test output"))
        return 1, tests, metrics, commands, fixtures
    return (0 if rc == 0 else 1), tests, metrics, commands, fixtures


# ── main ────────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="Modes: cpu (A1 unit tests), gpu (A2-A5 correctness groups via "
        "gpu_proofs_gate.py), catalog (A6, --filter rt_dynamic_catalog), "
        "export (A7, journey-proofs, needs ffmpeg/ffprobe), perf (A9, release "
        "build, needs --reference-project and --held-out-project). Groups "
        "whose tests are not yet implemented report blocked and exit nonzero. "
        "Exit 0 = all selected groups passed, 1 = failure/blocked/missing, "
        "2 = the runner could not complete.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--manifest-path", type=Path, default=None,
        help="Path to the workspace Cargo.toml (default: repo root next to scripts/)",
    )
    parser.add_argument(
        "--mode", required=True, choices=["cpu", "gpu", "catalog", "export", "perf"],
        help="acceptance mode to run (see description)",
    )
    parser.add_argument(
        "--report", type=Path, required=True,
        help="Path to write the JSON acceptance report (A0 schema)",
    )
    parser.add_argument(
        "--reference-project", type=Path, default=None,
        help="perf mode: reference-scene project file (SHA-256 recorded)",
    )
    parser.add_argument(
        "--held-out-project", type=Path, default=None,
        help="perf mode: held-out-scene project file (SHA-256 recorded)",
    )
    args = parser.parse_args()

    manifest = args.manifest_path or default_manifest_path()
    repo = manifest.resolve().parent

    sha, dirty = git_info(repo)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    artifact_dir = Path("/tmp/manifold-rt-dynamic") / sha / f"{args.mode}-{stamp}"

    report = {
        "schemaVersion": 1,
        "gitSha": sha,
        "dirtyPaths": dirty,
        "hardware": probe_hardware(),
        "osVersion": probe_os(),
        "mode": args.mode,
        "fixtures": [],
        "tests": [],
        "metrics": {},
        "commands": [],
        "exitCode": 2,
    }
    if args.mode in ("gpu", "catalog", "perf"):
        report["gpu"] = gpu_report_section(probe_gpu_family())

    try:
        if args.mode in ("gpu", "catalog", "perf"):
            ok, reason = host_gpu_available()
            if not ok:
                report["tests"].append(blocked_entry(
                    "host: native Metal RT", reason, "native Metal RT host"))
                report["metrics"] = {"passed": 0, "failed": 0, "blocked": 1}
                report["exitCode"] = 1
                write_report(args.report, report)
                log(f"\nRT DYNAMIC ACCEPTANCE ({args.mode}): BLOCKED — {reason}")
                return 1

        if args.mode == "cpu":
            exit_code, tests, metrics, commands, fixtures = mode_cpu(repo, manifest, artifact_dir)
        elif args.mode == "gpu":
            exit_code, tests, metrics, commands, fixtures = mode_gpu(repo, manifest, artifact_dir)
        elif args.mode == "catalog":
            exit_code, tests, metrics, commands, fixtures = mode_catalog(repo, manifest, artifact_dir)
        elif args.mode == "export":
            exit_code, tests, metrics, commands, fixtures = mode_export(repo, manifest, artifact_dir)
        else:  # perf
            exit_code, tests, metrics, commands, fixtures = mode_perf(
                repo, manifest, artifact_dir, args.reference_project, args.held_out_project)

        report["tests"] = tests
        report["metrics"] = metrics
        report["commands"] = commands
        report["fixtures"] = fixtures
        report["exitCode"] = exit_code
    except (OSError, subprocess.TimeoutExpired) as exc:
        report["tests"].append(blocked_entry(
            "runner", f"could not complete: {exc}", "completed dispatch"))
        report["exitCode"] = 2
        write_report(args.report, report)
        log(f"\nRT DYNAMIC ACCEPTANCE ({args.mode}): ERROR — {exc}")
        return 2

    write_report(args.report, report)
    blocked = sum(1 for t in tests if t["status"] == "blocked")
    failed = sum(1 for t in tests if t["status"] == "fail")
    if exit_code == 0:
        log(f"\nRT DYNAMIC ACCEPTANCE ({args.mode}): PASS "
            f"({len(tests)} tests recorded)")
    else:
        log(f"\nRT DYNAMIC ACCEPTANCE ({args.mode}): FAIL "
            f"({failed} failed, {blocked} blocked)")
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
