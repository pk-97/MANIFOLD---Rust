#!/usr/bin/env python3
"""Scene-modifier RT acceptance runner (SCENE_MODIFIER_RT_ACCEPTANCE.md A0/A10).

Dispatches the acceptance modes of the dynamic-RT-on-modified-meshes campaign
and writes one evidence report per run. This script is a dispatcher, not a
second test framework: every mode runs an existing cargo or gate entry point,
parses that run's own output, and records it. It never re-derives a verdict
the underlying harness did not produce.

MODES
  cpu      A1 mesh-change contract: `cargo test -p manifold-renderer mesh_change_`
  gpu      A2-A5 correctness groups via one scripts/gpu_proofs_gate.py run
           with repeated --filter arguments (baseline, fusion, ordering,
           shading, current_frame, refit — catalog and perf are excluded here
           per A0)
  catalog  A6 stock-catalog group: gpu_proofs_gate.py --filter rt_dynamic_catalog
  export   A7 production export: cargo test -p manifold-app --features
           journey-proofs rt_dynamic_export_ -- --test-threads=1
           (requires ffmpeg and ffprobe on PATH)
  perf     A9 bounded performance: explicitly invoked release-build proof,
           `cargo test --release -p manifold-renderer --features rt-perf-proofs
           --test gpu_proofs -- rt_dynamic_perf --test-threads=1`, followed by
           the held-out and reference-content app measurements.
           Requires --reference-project and --held-out-project; both files'
           SHA-256 hashes are recorded in the report fixtures. Pass
           --static-baseline-report with the measured pre-change report to
           qualify the static RT regression gate.

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
    if metrics["ignored"]:
        return 1, tests, metrics, commands, fixtures
    return (0 if rc == 0 else 1), tests, metrics, commands, fixtures


def _list_gpu_tests(repo: Path, artifact_dir: Path,
                    release: bool = False,
                    feature: str = "gpu-proofs") -> tuple[set[str] | None, int, dict]:
    """List the gpu_proofs test names. Returns (names, exit, command record);
    names is None when the listing itself failed."""
    list_cmd = ["cargo", "test"]
    if release:
        list_cmd.append("--release")
    list_cmd += ["-p", "manifold-renderer", "--features", feature,
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
    """Shared dispatch for gpu/catalog group runs through gpu_proofs_gate.py.
    GPU correctness groups share one gate invocation. Perf dispatches its
    release build itself instead
    (A9: one bounded run, never a debug-profile measurement)."""
    tests, commands = [], []
    listed, rc, record = _list_gpu_tests(repo, artifact_dir)
    commands.append(record)
    if listed is None:
        return 2, [blocked_entry(f"gpu: {', '.join(groups)}",
                                 f"listing exited {rc}", "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands

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
        if not group_tests:
            # Gate ran but per-test lines didn't parse: record the combined
            # gate as one entry, never a silent pass.
            group_tests = [{
                "name": ", ".join(selected_filters) + " (group)",
                "status": "pass" if rc == 0 else "fail",
                "observed": f"gate exit {rc}", "required": "parseable test output",
                "artifactPaths": [str(run_log)],
            }]
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


def _list_named_tests(cmd: list[str], repo: Path, log_path: Path) -> tuple[set[str], int, float]:
    """List one cargo test target without coupling app tests to GPU proof names."""
    return list_tests(cmd + ["--", "--list"], repo, log_path)


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


def _bool_fields(report: dict, fields: tuple[tuple[str, ...], ...]) -> tuple[bool, list[str]]:
    missing = []
    failed = []
    for path in fields:
        value = _nested(report, *path)
        label = ".".join(path)
        if not isinstance(value, bool):
            missing.append(label)
        elif not value:
            failed.append(label)
    return not missing and not failed, missing + failed


def _perf_qualification(
    reference: dict,
    held_out: dict,
    reference_content: dict | None = None,
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

    enforced_fields = (
        ("enforced", "completeFrames"),
        ("enforced", "noGpuFaults"),
        ("enforced", "zeroPostWarmupBufferAllocations"),
        ("enforced", "zeroPostWarmupAccelerationStructureAllocations"),
        ("enforced", "rtDispatched"),
        ("enforced", "profilingClean"),
    )
    held_out_ok, held_out_problems = _bool_fields(held_out, enforced_fields)
    if not held_out_ok:
        problems.extend(f"heldOut.{field}" for field in held_out_problems)
    if held_out.get("status") != "measured":
        problems.append("heldOut.status")

    reference_content_cpu_wall = None
    if reference_content is not None:
        reference_content_ok, reference_content_problems = _bool_fields(
            reference_content, enforced_fields)
        if not reference_content_ok:
            problems.extend(f"referenceContent.{field}" for field in reference_content_problems)
        if reference_content.get("status") != "measured":
            problems.append("referenceContent.status")
        value = _nested(reference_content, "cpuWall", "p95Ms")
        if isinstance(value, (int, float)):
            reference_content_cpu_wall = float(value)
        else:
            problems.append("referenceContent.cpuWall.p95Ms")

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

    held_out_cpu_wall = _nested(held_out, "cpuWall", "p95Ms")
    live_values = {
        "fullGpuFrameP95Ms": array_number(production, "dynamic_selective_refit", "gpuFrame"),
        # The renderer proof's cpuEncode is only its production renderer
        # section; use the separate full-content measurement below.
        # The reference content harness is the only source for the full
        # content-thread frame gate. Held-out cpuWall is report-only.
        "contentFrameP95Ms": reference_content_cpu_wall,
        "dynamicAsMaintenanceP95Ms": array_number(reference.get("configurations"), "dynamic_selective_refit", "gpuAsMaintenance"),
    }
    live_budget = "pass"
    for label, value in live_values.items():
        limit = 2.0 if label == "dynamicAsMaintenanceP95Ms" else 16.67
        if value is None:
            live_budget = "not_evaluated"
        elif value > limit:
            live_budget = "fail"
    qualification = {
        "correctness": "pass" if not problems else "fail",
        "resource": "pass" if not problems else "fail",
        "liveBudget": live_budget,
        "staticBaseline": static_baseline,
        "overall": ("fail" if problems or live_budget == "fail" or static_baseline == "fail"
                    else "pass" if live_budget == "pass" and static_baseline == "pass"
                    else "blocked"),
        "heldOutContentFrameP95Ms": (float(held_out_cpu_wall)
                                      if isinstance(held_out_cpu_wall, (int, float))
                                      else None),
        "referenceContentFrameP95Ms": reference_content_cpu_wall,
    }
    return qualification, problems


def mode_perf(repo: Path, manifest: Path, artifact_dir: Path,
              reference_project: Path | None, held_out_project: Path | None,
              static_baseline_report: Path | None = None):
    fixtures = []
    tests = []
    commands = []
    missing_inputs = []
    for label, path in (("reference-project", reference_project),
                        ("held-out-project", held_out_project)):
        if path is None:
            missing_inputs.append(f"{label} not given")
        elif not Path(path).exists():
            missing_inputs.append(f"{label} not found: {path}")
        else:
            fixtures.append({"name": Path(path).name, "hash": sha256_file(Path(path))})
    if static_baseline_report is not None and static_baseline_report.is_file():
        fixtures.append({"name": static_baseline_report.name,
                         "hash": sha256_file(static_baseline_report)})
    if missing_inputs:
        # A9: no invented proxy pass — a missing held-out fixture blocks.
        tests.append(blocked_entry(
            "perf: inputs", "; ".join(missing_inputs),
            "--reference-project and --held-out-project"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    reference_project = Path(reference_project)
    held_out_project = Path(held_out_project)
    checked_in_reference = repo / "crates/manifold-renderer/tests/fixtures/scene-modifiers/rt_dynamic_reference.json"
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

    held_out_base = ["cargo", "test", "--release", "-p", "manifold-app",
                     "--features", "journey-proofs,perf-soak", "rt_dynamic_held_out::rt_dynamic_held_out"]
    held_out_list_log = artifact_dir / "list-held-out.log"
    held_out_listed, held_out_rc, held_out_list_duration = _list_named_tests(
        held_out_base, repo, held_out_list_log)
    commands.append({"cmd": " ".join(held_out_base + ["--", "--list"]),
                     "exitCode": held_out_rc,
                     "durationSec": round(held_out_list_duration, 1),
                     "log": str(held_out_list_log)})
    if held_out_rc != 0:
        return 2, [blocked_entry("perf: rt_dynamic_held_out",
                                 f"listing exited {held_out_rc}",
                                 "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures
    if not any("rt_dynamic_held_out" in name for name in held_out_listed):
        tests.append(blocked_entry(
            "rt_dynamic_held_out", "0 tests listed for 'rt_dynamic_held_out'",
            "tests not yet implemented"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    reference_content_base = [
        "cargo", "test", "--release", "-p", "manifold-app",
        "--features", "journey-proofs,perf-soak", "rt_dynamic_reference_content",
    ]
    reference_content_list_log = artifact_dir / "list-reference-content.log"
    reference_content_listed, reference_content_rc, reference_content_list_duration = _list_named_tests(
        reference_content_base, repo, reference_content_list_log)
    commands.append({"cmd": " ".join(reference_content_base + ["--", "--list"]),
                     "exitCode": reference_content_rc,
                     "durationSec": round(reference_content_list_duration, 1),
                     "log": str(reference_content_list_log)})
    if reference_content_rc != 0:
        return 2, [blocked_entry("perf: rt_dynamic_reference_content",
                                 f"listing exited {reference_content_rc}",
                                 "listable test suite")], {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures
    if not any("rt_dynamic_reference_content" in name for name in reference_content_listed):
        tests.append(blocked_entry(
            "rt_dynamic_reference_content", "0 tests listed for 'rt_dynamic_reference_content'",
            "tests not yet implemented"))
        return 1, tests, {"passed": 0, "failed": 0, "blocked": 1}, commands, fixtures

    reference_report = artifact_dir / "rt_dynamic_perf.json"
    held_out_report = artifact_dir / "held-out.json"
    reference_content_report = artifact_dir / "reference-content.json"
    env = {
        "MANIFOLD_RT_REFERENCE_PROJECT": reference_project,
        "MANIFOLD_RT_HELD_OUT_PROJECT": held_out_project,
        "MANIFOLD_RT_PERF_REPORT": reference_report,
        "MANIFOLD_RT_REFERENCE_REPORT": reference_report,
        "MANIFOLD_RT_HELD_OUT_REPORT": held_out_report,
        "MANIFOLD_RT_REFERENCE_CONTENT_REPORT": reference_content_report,
    }
    run_log = artifact_dir / "run-rt_dynamic_perf-release.log"
    run_cmd = ["cargo", "test", "--release", "-p", "manifold-renderer",
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

    held_out_log = artifact_dir / "run-rt_dynamic_held_out-release.log"
    held_out_cmd = held_out_base + ["--", "--test-threads=1"]
    held_out_rc, held_out_dur = run_streamed(held_out_cmd, repo, held_out_log, env=env)
    commands.append({"cmd": " ".join(held_out_cmd), "exitCode": held_out_rc,
                     "durationSec": round(held_out_dur, 1), "log": str(held_out_log),
                     "env": {key: str(value) for key, value in env.items()}})
    held_out_output = held_out_log.read_text(errors="replace") if held_out_log.exists() else ""
    held_out_tests = parse_test_results(held_out_output)
    for test in held_out_tests:
        test["artifactPaths"] = [str(held_out_log)]
    tests.extend(held_out_tests)
    held_out_summary = parse_result_summary(held_out_output)
    if held_out_rc == 0 and not held_out_tests:
        tests.append(blocked_entry(
            "perf: rt_dynamic_held_out", "run passed but no per-test results parsed",
            "parseable test output"))
        held_out_rc = 1

    reference_content_log = artifact_dir / "run-rt_dynamic_reference_content-release.log"
    reference_content_cmd = reference_content_base + ["--", "--test-threads=1"]
    reference_content_rc, reference_content_duration = run_streamed(
        reference_content_cmd, repo, reference_content_log, env=env)
    commands.append({"cmd": " ".join(reference_content_cmd),
                     "exitCode": reference_content_rc,
                     "durationSec": round(reference_content_duration, 1),
                     "log": str(reference_content_log),
                     "env": {key: str(value) for key, value in env.items()}})
    reference_content_output = reference_content_log.read_text(errors="replace") if reference_content_log.exists() else ""
    reference_content_tests = parse_test_results(reference_content_output)
    for test in reference_content_tests:
        test["artifactPaths"] = [str(reference_content_log)]
    tests.extend(reference_content_tests)
    reference_content_summary = parse_result_summary(reference_content_output)
    if reference_content_rc == 0 and not reference_content_tests:
        tests.append(blocked_entry(
            "perf: rt_dynamic_reference_content",
            "run passed but no per-test results parsed",
            "parseable test output"))
        reference_content_rc = 1

    reference_report_data, reference_error = _load_json_report(reference_report, "reference")
    held_out_report_data, held_out_error = _load_json_report(held_out_report, "held-out")
    reference_content_report_data, reference_content_error = _load_json_report(
        reference_content_report, "reference-content")
    tests.append(_report_test("perf: reference report", reference_report_data,
                              reference_error, reference_report))
    tests.append(_report_test("perf: held-out report", held_out_report_data,
                              held_out_error, held_out_report))
    tests.append(_report_test("perf: reference content report",
                              reference_content_report_data,
                              reference_content_error, reference_content_report))
    report_errors = [error for error in (
        reference_error, held_out_error, reference_content_error) if error]
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
    if held_out_report_data:
        if not held_out_report_data.get("projectHash"):
            report_errors.append("held-out report is missing projectHash")
        elif held_out_report_data["projectHash"] != sha256_file(held_out_project):
            report_errors.append("held-out report project hash does not match supplied project")
    if reference_content_report_data:
        if not reference_content_report_data.get("projectHash"):
            report_errors.append("reference content report is missing projectHash")
        elif reference_content_report_data["projectHash"] != expected_hash:
            report_errors.append("reference content report project hash does not match checked-in graph")

    qualification = None
    qualification_problems = []
    if not report_errors and reference_report_data and held_out_report_data and reference_content_report_data:
        qualification, qualification_problems = _perf_qualification(
            reference_report_data, held_out_report_data,
            reference_content_report_data, static_regression)
        if qualification_problems:
            tests.append(blocked_entry(
                "perf: correctness/resource evidence",
                "; ".join(qualification_problems),
                "reference and held-out correctness/resource fields"))

    if qualification and qualification["overall"] == "blocked":
        tests.append(blocked_entry("perf: full live qualification",
            f"live budget={qualification["liveBudget"]}; static baseline={qualification["staticBaseline"]}",
            "measured reference live budget and pre-change static baseline"))

    metrics = {
        "passed": (reference_summary["passed"] + held_out_summary["passed"]
                   + reference_content_summary["passed"]),
        "failed": (reference_summary["failed"] + held_out_summary["failed"]
                   + reference_content_summary["failed"]),
        "ignored": (reference_summary["ignored"] + held_out_summary["ignored"]
                    + reference_content_summary["ignored"]),
        "blocked": sum(1 for test in tests if test["status"] == "blocked"),
        "sourceHashes": {
            "checkedInReference": expected_hash,
            "suppliedReference": reference_hash,
            "suppliedHeldOut": sha256_file(held_out_project),
            "reportedHeldOut": held_out_report_data.get("projectHash") if held_out_report_data else None,
            "reportedReferenceContent": reference_content_report_data.get("projectHash") if reference_content_report_data else None,
        },
        "qualification": qualification or {
            "correctness": "not_evaluated",
            "resource": "not_evaluated",
            "liveBudget": "not_evaluated",
            "staticBaseline": static_regression.get("status", "not_evaluated"),
            "overall": "fail",
            "heldOutContentFrameP95Ms": None,
            "referenceContentFrameP95Ms": None,
        },
        "staticRegression": static_regression,
        "achieved": {
            "heldOutContentFrameP95Ms": (
                float(_nested(held_out_report_data, "cpuWall", "p95Ms"))
                if held_out_report_data
                and isinstance(_nested(held_out_report_data, "cpuWall", "p95Ms"), (int, float))
                else None
            ),
            "referenceContentFrameP95Ms": (
                float(_nested(reference_content_report_data, "cpuWall", "p95Ms"))
                if reference_content_report_data
                and isinstance(_nested(reference_content_report_data, "cpuWall", "p95Ms"), (int, float))
                else None
            ),
        },
        "gateStatuses": {
            "reference": reference_report_data.get("gates") if reference_report_data else None,
            "referenceConfigurations": {
                entry.get("name"): entry.get("status")
                for entry in (reference_report_data.get("productionConfigurations", [])
                              if reference_report_data else [])
                if isinstance(entry, dict)
            },
            "heldOut": held_out_report_data.get("enforced") if held_out_report_data else None,
            "referenceContent": reference_content_report_data.get("enforced") if reference_content_report_data else None,
            "staticRegression": static_regression,
        },
        "referenceReport": reference_report_data,
        "heldOutReport": held_out_report_data,
        "referenceContentReport": reference_content_report_data,
    }
    if static_test_status == "pass":
        metrics["passed"] += 1
    elif static_test_status == "fail":
        metrics["failed"] += 1
    else:
        metrics["blocked"] += 1
    exit_code = 0
    if (rc != 0 or held_out_rc != 0 or reference_content_rc != 0
            or metrics["ignored"] or report_errors):
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
                args.held_out_project, args.static_baseline_report)

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
