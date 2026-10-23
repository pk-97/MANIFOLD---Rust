#!/usr/bin/env python3
"""Scene-modifier RT acceptance runner (SCENE_MODIFIER_RT_ACCEPTANCE.md A0/A10).

Dispatches the acceptance modes of the dynamic-RT-on-modified-meshes campaign
and writes one evidence report per run. This script is a dispatcher, not a
second test framework: every mode runs an existing cargo or gate entry point,
parses that run's own output, and records it. It never re-derives a verdict
the underlying harness did not produce.

MODES
  cpu      A1 mesh-change contract: `cargo test -p manifold-nodes mesh_change_`
  gpu      A2-A5 correctness groups via one scripts/gpu_proofs_gate.py run
           with repeated --filter arguments (baseline, fusion, ordering,
           shading, current_frame, refit — catalog and perf are excluded here
           per A0)
  catalog  A6 stock-catalog group: gpu_proofs_gate.py --filter rt_dynamic_catalog
  export   A7 production export: cargo test -p manifold-app --features
           journey-proofs rt_dynamic_export_ -- --test-threads=1
           (requires ffmpeg and ffprobe on PATH)
  perf     A9 bounded performance: explicitly invoked release-build proof,
           `cargo test --release -p manifold-nodes --features rt-perf-proofs
           --test gpu_proofs -- rt_dynamic_perf --test-threads=1`.
           Requires --reference-project; its SHA-256 hash is recorded in the
           report fixtures. Pass --static-baseline-report with the measured
           pre-change report to qualify the static RT regression gate. The
           content-thread frame budget has no harness since the app's
           held-out and reference-content measurements went with the old
           scene path, so the live budget reports not_evaluated and the
           overall verdict stays blocked until one exists.

Every required group has a dispatch entry. Absence is detected by listing tests
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
      --reference-project ref.manifold \
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
    "rt_dynamic_oracle",
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


def run_streamed(
    cmd: list[str], cwd: Path, log_path: Path,
    env: dict[str, str] | None = None,
) -> tuple[int, float]:
    """Stream a command's merged output to the console and a log file.

    Returns (exit code, duration seconds). A timeout or spawn failure raises —
    the caller converts that into a runner-level exit 2."""
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log(f"$ {' '.join(map(str, cmd))}")
    start = time.time()
    with open(log_path, "w") as logf:
        process_env = os.environ.copy()
        if env:
            process_env.update({key: str(value) for key, value in env.items()})
        proc = subprocess.Popen(
            cmd,
            cwd=str(cwd),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            env=process_env,
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
            tests.append({
                "name": name,
                # A0 forbids muted acceptance gates. Keep ignored tests in the
                # report as failures so a passing subset cannot hide one.
                "status": "pass" if outcome == "ok" else "fail",
                "observed": outcome,
                "required": "selected non-ignored test" if outcome == "ignored" else None,
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


def _number(value) -> bool:
    return (isinstance(value, (int, float)) and not isinstance(value, bool)
            and value == value and value not in (float("inf"), float("-inf")))


def _static_result(status: str, reason: str, **fields) -> dict:
    result = {"status": status, "reason": reason}
    result.update(fields)
    return result


def validate_static_baseline(baseline_path: Path | None,
                             current_report: dict | None,
                             current_report_path: Path | None = None) -> dict:
    """Compare a measured pre-change report with this run's static frame."""
    provenance = {
        "baselineReport": str(baseline_path) if baseline_path else None,
        "currentReport": str(current_report_path) if current_report_path else None,
    }
    if baseline_path is None:
        return _static_result("blocked", "static baseline report was not provided",
                              provenance=provenance)
    if not baseline_path.exists():
        return _static_result("blocked", f"static baseline report not found: {baseline_path}",
                              provenance=provenance)
    try:
        baseline = json.loads(baseline_path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        return _static_result("fail", f"static baseline report is malformed: {exc}",
                              provenance=provenance)
    if not isinstance(baseline, dict):
        return _static_result("fail", "static baseline report root must be an object",
                              provenance=provenance)
    if current_report is None:
        return _static_result("blocked", "current production performance report is missing",
                              provenance=provenance)

    if not isinstance(current_report, dict):
        return _static_result("fail", "current report root must be an object", provenance=provenance)
    for report, name, fields in (
        (baseline, "baseline", ("settings", "gpuFrame", "rtDispatchWitness", "fixture", "hardware")),
        (current_report, "current", ("hardware", "fixture")),
    ):
        for field in fields:
            if field in report and not isinstance(report[field], dict):
                return _static_result("fail", f"{name}.{field} must be an object", provenance=provenance)
    configs = current_report.get("productionConfigurations", [])
    if not isinstance(configs, list):
        return _static_result("fail", "productionConfigurations must be an array", provenance=provenance)
    for config in configs:
        if not isinstance(config, dict) or not isinstance(config.get("gpuFrame"), dict):
            return _static_result("fail", "configuration/gpuFrame must be objects", provenance=provenance)

    baseline_settings = baseline.get("settings")
    baseline_frame = baseline.get("gpuFrame")
    baseline_witness = baseline.get("rtDispatchWitness")
    baseline_fixture = baseline.get("fixture")
    current_hardware = current_report.get("hardware")
    current_configs = current_report.get("productionConfigurations")
    current_static = next((item for item in current_configs or []
                           if isinstance(item, dict) and item.get("name") == "static_rt"), None)
    problems = []
    if baseline.get("status") != "measured_static_rt":
        problems.append("baseline status is not measured_static_rt")
    if (not isinstance(baseline_witness, dict)
            or baseline_witness.get("observed") is not True
            or not baseline_witness.get("channels")):
        problems.append("baseline has no observed RT dispatch witness")
    if not isinstance(baseline_settings, dict) or baseline_settings.get("measuredFrames") != 120:
        problems.append("baseline measuredFrames is not 120")
    if (not isinstance(baseline_frame, dict)
            or baseline_frame.get("count") != 120
            or not _number(baseline_frame.get("p95Ms"))
            or baseline_frame["p95Ms"] <= 0):
        problems.append("baseline gpuFrame does not contain 120 timed samples")
    if (not isinstance(baseline_settings, dict)
            or baseline_settings.get("resolution") != [1280, 720]):
        problems.append("baseline resolution is not 1280x720")
    baseline_gpu = (baseline.get("hardware") or {}).get("gpu")
    baseline_hash = ((baseline_fixture or {}).get("sha256")
                     if isinstance(baseline_fixture, dict) else None)
    if not baseline_gpu:
        problems.append("baseline GPU identity is missing")
    if not baseline_hash:
        problems.append("baseline fixture hash is missing")
    if not isinstance(current_static, dict):
        problems.append("current production report has no static_rt configuration")
    if not isinstance(current_hardware, dict) or not current_hardware.get("gpu"):
        problems.append("current production GPU identity is missing")
    current_hash = current_report.get("referenceProjectHash")
    if not current_hash:
        current_hash = (current_report.get("fixture") or {}).get("sha256")
    if not current_hash:
        problems.append("current production fixture hash is missing")
    if current_static:
        if current_static.get("status") != "measured_production_frame":
            problems.append("current static_rt status is not measured_production_frame")
        current_frame = current_static.get("gpuFrame") or {}
        if (current_static.get("measuredFrames") != 120
                or current_frame.get("count") != 120):
            problems.append("current static_rt does not contain 120 timed samples")
        if current_static.get("resolution") != [1280, 720]:
            problems.append("current static_rt resolution is not 1280x720")
        if (current_static.get("anyDispatch") is not True
                or current_static.get("dispatchFrames") != 120):
            problems.append("current static_rt has no RT dispatch evidence")
        if not _number(current_frame.get("p95Ms")) or current_frame["p95Ms"] <= 0:
            problems.append("current static_rt gpuFrame has no p95")
    if baseline_gpu and isinstance(current_hardware, dict) and baseline_gpu != current_hardware.get("gpu"):
        problems.append(f"GPU mismatch: baseline {baseline_gpu!r}, current {current_hardware.get('gpu')!r}")
    if baseline_hash and current_hash and baseline_hash != current_hash:
        problems.append("fixture hash mismatch between baseline and current production report")
    if problems:
        return _static_result(
            "fail", "; ".join(problems), provenance=provenance,
            baselineSourceCommit=baseline.get("sourceCommit"),
            baselineGpuP95Ms=(baseline_frame or {}).get("p95Ms"),
            fixtureSha256=baseline_hash,
            resolution=(baseline_settings or {}).get("resolution"),
        )

    baseline_p95 = baseline_frame["p95Ms"]
    current_p95 = current_static["gpuFrame"]["p95Ms"]
    allowed_delta = max(baseline_p95 * 0.05, 0.2)
    delta = current_p95 - baseline_p95
    regression = delta > allowed_delta
    return _static_result(
        "fail" if regression else "pass",
        "current static RT p95 exceeds allowed regression" if regression
        else "static RT p95 is within baseline tolerance",
        provenance=provenance,
        baselineSourceCommit=baseline.get("sourceCommit"),
        baselineGpu=baseline_gpu,
        currentGpu=current_hardware.get("gpu"),
        fixtureSha256=baseline_hash,
        resolution=[1280, 720],
        baselineGpuP95Ms=baseline_p95,
        currentGpuP95Ms=current_p95,
        deltaMs=delta,
        allowedDeltaMs=allowed_delta,
        toleranceRule="max(5% of baseline p95, 0.2 ms)",
    )


# ── mode implementations ────────────────────────────────────────────────
#
# Each returns (exit_code, tests, metrics, commands, fixtures). Dispatch is
# table-driven at the bottom; a mode whose tests are absent still has its
# entry here and reports blocked rather than failing to parse.


def mode_cpu(repo: Path, manifest: Path, artifact_dir: Path):
    tests, commands = [], []
    fixtures = []
    list_cmd = ["cargo", "test", "-p", "manifold-nodes", "mesh_change_", "--", "--list"]
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
    run_cmd = ["cargo", "test", "-p", "manifold-nodes", "mesh_change_"]
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
    if metrics["ignored"]:
        return 1, tests, metrics, commands, fixtures
    return (0 if rc == 0 else 1), tests, metrics, commands, fixtures


def _list_gpu_tests(repo: Path, artifact_dir: Path,
                    release: bool = False,
                    feature: str = "gpu-proofs",
                    package: str = "manifold-nodes-scene") -> tuple[set[str] | None, int, dict]:
    """List the gpu_proofs test names. Returns (names, exit, command record);
    names is None when the listing itself failed."""
    list_cmd = ["cargo", "test"]
    if release:
        list_cmd.append("--release")
    list_cmd += ["-p", package, "--features", feature,
                 "--test", "gpu_proofs", "--", "--list"]
    list_log = artifact_dir / f"list-{package}.log"
    listed, rc, dur = list_tests(list_cmd, repo, list_log)
    record = {"cmd": " ".join(list_cmd), "exitCode": rc,
              "durationSec": round(dur, 1), "log": str(list_log)}
    if rc != 0:
        return None, rc, record
    return listed, rc, record


def _mode_gpu_groups(repo: Path, manifest: Path, artifact_dir: Path,
                     groups: list[str]):
    """Shared dispatch for gpu/catalog group runs through gpu_proofs_gate.py.
    GPU correctness groups share one gate invocation. Perf dispatches its
    release build itself instead
    (A9: one bounded run, never a debug-profile measurement)."""
    tests, commands = [], []
    listed = set()
    for package in ("manifold-nodes", "manifold-nodes-scene"):
        names, rc, record = _list_gpu_tests(repo, artifact_dir, package=package)
        commands.append(record)
        if names is None:
            return 2, [blocked_entry(f"gpu: {', '.join(groups)}",
                                     f"{package} listing exited {rc}", "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands
        listed.update(names)

    metrics = {"passed": 0, "failed": 0, "ignored": 0, "blocked": 0}
    exit_code = 0
    selected_filters = []
    for group in groups:
        matched = sorted(t for t in listed if group in t)
        if not matched:
            tests.append(blocked_entry(
                f"{group}", f"0 tests listed for '{group}'",
                "tests not yet implemented"))
            metrics["blocked"] += 1
            exit_code = 1
        else:
            selected_filters.append(group)

    # Keep correctness groups in one gate invocation. Besides making the
    # report atomic, this preserves gpu_proofs_gate's serial-device discipline
    # and avoids rebuilding/reacquiring the native device once per filter.
    if selected_filters:
        run_log = artifact_dir / "run-gpu-correctness.log"
        gate = Path(__file__).resolve().parent / "gpu_proofs_gate.py"
        run_cmd = ["python3", str(gate), "--manifest-path", str(manifest)]
        for group in selected_filters:
            run_cmd.extend(["--filter", group])
        rc, dur = run_streamed(run_cmd, repo, run_log)
        commands.append({"cmd": " ".join(run_cmd), "exitCode": rc,
                         "durationSec": round(dur, 1), "log": str(run_log)})
        output = run_log.read_text(errors="replace") if run_log.exists() else ""
        group_tests = parse_test_results(output)
        for t in group_tests:
            t["artifactPaths"] = [str(run_log)]
        for group in selected_filters:
            if not any(group in test["name"] for test in group_tests):
                group_tests.append(blocked_entry(
                    group, "run selected zero tests or no per-test results parsed",
                    "selected non-ignored test"))
                metrics["blocked"] += 1
                exit_code = 1
        summary = parse_result_summary(output)
        metrics["passed"] += summary["passed"]
        metrics["failed"] += summary["failed"]
        metrics["ignored"] += summary["ignored"]
        if rc != 0 or summary["ignored"]:
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


def _load_json_report(path: Path, label: str) -> tuple[dict | None, str | None]:
    if not path.is_file():
        return None, f"{label} report is missing: {path}"
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        return None, f"{label} report is malformed: {exc}"
    if not isinstance(value, dict):
        return None, f"{label} report must contain a JSON object"
    return value, None


def _report_test(name: str, report: dict | None, error: str | None,
                 path: Path) -> dict:
    if error:
        return {
            "name": name,
            "status": "fail",
            "observed": error,
            "required": "complete JSON report",
            "artifactPaths": [str(path)],
        }
    return {
        "name": name,
        "status": "pass",
        "observed": report.get("status", "report loaded") if report else "report loaded",
        "required": "complete JSON report",
        "artifactPaths": [str(path)],
    }


def _nested(value: object, *keys: str) -> object | None:
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


def _perf_qualification(
    reference: dict,
    static_regression: dict | None = None,
) -> tuple[dict, list[str]]:
    """Classify measured perf evidence without turning absent baselines into passes."""
    problems: list[str] = []
    production = reference.get("productionConfigurations")
    if not isinstance(production, list):
        problems.append("reference.productionConfigurations")
        production = []
    production_statuses = {
        item.get("name"): item.get("status")
        for item in production if isinstance(item, dict)
    }
    required_production = {
        "static_rt", "dynamic_selective_refit", "fresh_build_reference"
    }
    missing_production = sorted(required_production - production_statuses.keys())
    if missing_production:
        problems.extend(f"reference.productionConfigurations.{name}" for name in missing_production)
    bad_production = sorted(
        name for name in required_production & production_statuses.keys()
        if production_statuses[name] != "measured_production_frame"
    )
    if bad_production:
        problems.extend(f"reference.{name}.status" for name in bad_production)

    # These gates are deliberately separate from correctness/resource status.
    # The optional pre-change report is evaluated by mode_perf and supplied
    # here as a structured result when available.
    static_gate = _nested(reference, "gates", "staticRegression")
    if static_regression is not None:
        static_baseline = static_regression.get("status", "fail")
    else:
        static_baseline = "missing" if static_gate else "not_evaluated"
        if isinstance(static_gate, dict):
            static_baseline = "pass" if static_gate.get("passed") is True else "fail"
        elif isinstance(static_gate, (int, float)):
            static_baseline = "pass" if static_gate <= 0.05 else "fail"

    # The reports currently serialize the configuration collection as an array.
    def array_number(collection: object, name: str, section: str) -> float | None:
        if not isinstance(collection, list):
            return None
        item = next((entry for entry in collection
                     if isinstance(entry, dict) and entry.get("name") == name), None)
        value = _nested(item, section, "p95Ms")
        return float(value) if isinstance(value, (int, float)) else None

    live_values = {
        "fullGpuFrameP95Ms": array_number(production, "dynamic_selective_refit", "gpuFrame"),
        # The renderer proof's cpuEncode is only its production renderer
        # section, not the full content-thread frame, which has no harness.
        "contentFrameP95Ms": None,
        "dynamicAsMaintenanceP95Ms": array_number(reference.get("configurations"), "dynamic_selective_refit", "gpuAsMaintenance"),
    }
    live_budget = "pass"
    for label, value in live_values.items():
        limit = 2.0 if label == "dynamicAsMaintenanceP95Ms" else 16.67
        # A measured overrun fails even while another value is unmeasured.
        if value is not None and value > limit:
            live_budget = "fail"
        elif value is None and live_budget == "pass":
            live_budget = "not_evaluated"
    qualification = {
        "correctness": "pass" if not problems else "fail",
        "resource": "pass" if not problems else "fail",
        "liveBudget": live_budget,
        "staticBaseline": static_baseline,
        "overall": ("fail" if problems or live_budget == "fail" or static_baseline == "fail"
                    else "pass" if live_budget == "pass" and static_baseline == "pass"
                    else "blocked"),
    }
    return qualification, problems


def mode_perf(repo: Path, manifest: Path, artifact_dir: Path,
              reference_project: Path | None,
              static_baseline_report: Path | None = None):
    fixtures = []
    tests = []
    commands = []
    missing_inputs = []
    if reference_project is None:
        missing_inputs.append("reference-project not given")
    elif not Path(reference_project).exists():
        missing_inputs.append(f"reference-project not found: {reference_project}")
    else:
        fixtures.append({"name": Path(reference_project).name,
                         "hash": sha256_file(Path(reference_project))})
    if static_baseline_report is not None and static_baseline_report.is_file():
        fixtures.append({"name": static_baseline_report.name,
                         "hash": sha256_file(static_baseline_report)})
    if missing_inputs:
        # A9: no invented proxy pass — a missing reference fixture blocks.
        tests.append(blocked_entry(
            "perf: inputs", "; ".join(missing_inputs), "--reference-project"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    reference_project = Path(reference_project)
    checked_in_reference = repo / "crates/manifold-nodes-scene/tests/fixtures/scene-modifiers/rt_dynamic_reference.json"
    if not checked_in_reference.is_file():
        tests.append(blocked_entry(
            "perf: reference fixture",
            f"checked-in reference graph is missing: {checked_in_reference}",
            "checked-in reference graph"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures
    expected_hash = sha256_file(checked_in_reference)
    reference_hash = sha256_file(reference_project)
    fixtures.append({"name": "rt_dynamic_reference.json", "hash": expected_hash})
    if reference_hash != expected_hash:
        tests.append(blocked_entry(
            "perf: reference fixture hash",
            f"supplied reference hash {reference_hash} does not match checked-in graph {expected_hash}",
            "reference fixture hash matches checked-in graph"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    listed, rc, record = _list_gpu_tests(
        repo, artifact_dir, release=True, feature="rt-perf-proofs")
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

    reference_report = artifact_dir / "rt_dynamic_perf.json"
    env = {
        "MANIFOLD_RT_REFERENCE_PROJECT": reference_project,
        "MANIFOLD_RT_PERF_REPORT": reference_report,
        "MANIFOLD_RT_REFERENCE_REPORT": reference_report,
    }
    run_log = artifact_dir / "run-rt_dynamic_perf-release.log"
    run_cmd = ["cargo", "test", "--release", "-p", "manifold-nodes-scene",
               "--features", "rt-perf-proofs", "--test", "gpu_proofs", "--",
               "rt_dynamic_perf", "--test-threads=1"]
    rc, dur = run_streamed(run_cmd, repo, run_log, env=env)
    commands.append({"cmd": " ".join(run_cmd), "exitCode": rc,
                     "durationSec": round(dur, 1), "log": str(run_log),
                     "env": {key: str(value) for key, value in env.items()}})
    output = run_log.read_text(errors="replace") if run_log.exists() else ""
    tests.extend(parse_test_results(output))
    for t in tests:
        t["artifactPaths"] = [str(run_log)]
    reference_summary = parse_result_summary(output)
    if rc == 0 and not parse_test_results(output):
        tests.append(blocked_entry(
            "perf: rt_dynamic_perf", "run passed but no per-test results parsed",
            "parseable test output"))
        rc = 1

    reference_report_data, reference_error = _load_json_report(reference_report, "reference")
    tests.append(_report_test("perf: reference report", reference_report_data,
                              reference_error, reference_report))
    report_errors = [reference_error] if reference_error else []
    static_regression = validate_static_baseline(
        static_baseline_report, reference_report_data, reference_report)
    static_test_status = static_regression["status"]
    tests.append({
        "name": "perf: static RT regression",
        "status": static_test_status,
        "observed": static_regression.get("reason"),
        "required": "measured pre-change baseline on matching GPU and fixture",
        "artifactPaths": [str(path) for path in (
            static_baseline_report, reference_report) if path is not None],
    })
    if reference_report_data and reference_report_data.get("referenceProjectHash"):
        if reference_report_data["referenceProjectHash"] != expected_hash:
            report_errors.append("reference report source hash does not match checked-in graph")

    qualification = None
    qualification_problems = []
    if not report_errors and reference_report_data:
        qualification, qualification_problems = _perf_qualification(
            reference_report_data, static_regression)
        if qualification_problems:
            tests.append(blocked_entry(
                "perf: correctness/resource evidence",
                "; ".join(qualification_problems),
                "reference correctness/resource fields"))

    if qualification and qualification["overall"] == "blocked":
        tests.append(blocked_entry("perf: full live qualification",
            f"live budget={qualification["liveBudget"]}; static baseline={qualification["staticBaseline"]}",
            "measured reference live budget and pre-change static baseline"))

    metrics = {
        "passed": reference_summary["passed"],
        "failed": reference_summary["failed"],
        "ignored": reference_summary["ignored"],
        "blocked": sum(1 for test in tests if test["status"] == "blocked"),
        "sourceHashes": {
            "checkedInReference": expected_hash,
            "suppliedReference": reference_hash,
        },
        "qualification": qualification or {
            "correctness": "not_evaluated",
            "resource": "not_evaluated",
            "liveBudget": "not_evaluated",
            "staticBaseline": static_regression.get("status", "not_evaluated"),
            "overall": "fail",
        },
        "staticRegression": static_regression,
        "gateStatuses": {
            "reference": reference_report_data.get("gates") if reference_report_data else None,
            "referenceConfigurations": {
                entry.get("name"): entry.get("status")
                for entry in (reference_report_data.get("productionConfigurations", [])
                              if reference_report_data else [])
                if isinstance(entry, dict)
            },
            "staticRegression": static_regression,
        },
        "referenceReport": reference_report_data,
    }
    if static_test_status == "pass":
        metrics["passed"] += 1
    elif static_test_status == "fail":
        metrics["failed"] += 1
    else:
        metrics["blocked"] += 1
    exit_code = 0
    if rc != 0 or metrics["ignored"] or report_errors:
        exit_code = 1
    if qualification and qualification["overall"] != "pass":
        exit_code = 1
    if static_test_status == "fail":
        exit_code = 1
    elif static_test_status == "blocked":
        exit_code = 1
    return exit_code, tests, metrics, commands, fixtures


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
    if metrics["ignored"]:
        return 1, tests, metrics, commands, fixtures
    return (0 if rc == 0 else 1), tests, metrics, commands, fixtures


# ── main ────────────────────────────────────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="Modes: cpu (A1 unit tests), gpu (A2-A5 correctness groups via "
        "gpu_proofs_gate.py), catalog (A6, --filter rt_dynamic_catalog), "
        "export (A7, journey-proofs, needs ffmpeg/ffprobe), perf (A9, release "
        "build, needs --reference-project). Groups "
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
        "--static-baseline-report", type=Path, default=None,
        help="perf mode: measured pre-change static RT report (qualification baseline)",
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
                repo, manifest, artifact_dir, args.reference_project,
                args.static_baseline_report)

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
    import gpu_queue

    with gpu_queue.hold("rt_dynamic_acceptance"):
        sys.exit(main())
