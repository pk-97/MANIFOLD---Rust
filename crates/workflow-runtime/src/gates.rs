//! Gate execution: shell out, capture exit + tail. Heavy gates and the verdict
//! trail belong to `scripts/gate_runner.py` (WORKFLOW_RUNTIME_DESIGN.md D3) —
//! a program's gate line calls it; this module never re-implements it.
//!
//! INVARIANT (structural, rg-gated at landing): subprocess spawns live only
//! here, in `worktree.rs`, `lane.rs`, and `transport.rs`'s keyget.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const TAIL_LINES: usize = 20;
/// Timeout-as-FAIL, same policy as landing_gate. Generous: GPU builds are slow.
pub const DEFAULT_GATE_TIMEOUT_S: u64 = 900;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub cmd: String,
    pub exit: i32,
    pub tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateReport {
    pub results: Vec<GateResult>,
    pub pass: bool,
}

/// Run gate commands in order, stopping at the first failure. A command that
/// outlives the timeout is killed and FAILS (a hung gate must never hold an
/// overnight run — finding 5 of the 2026-07-29 adversarial review).
pub fn run_gates(cmds: &[String], cwd: &Path, timeout_s: u64, run_dir: &Path) -> GateReport {
    run_gates_env(cmds, cwd, timeout_s, run_dir, &[])
}

/// `extra_env` is how a gate sees per-invocation state it can't get from the
/// run dir — e.g. `$WORKFLOW_SAMPLE` pointing at a sample candidate.
pub fn run_gates_env(
    cmds: &[String],
    cwd: &Path,
    timeout_s: u64,
    run_dir: &Path,
    extra_env: &[(&str, &Path)],
) -> GateReport {
    let mut results = Vec::new();
    let mut pass = true;
    for cmd in cmds {
        crate::status::emit(run_dir, |st| {
            st.state = "gate".into();
            st.detail = cmd.clone();
        });
        let (exit, tail) = run_one(cmd, cwd, Duration::from_secs(timeout_s), run_dir, extra_env);
        let ok = exit == 0;
        results.push(GateResult { cmd: cmd.clone(), exit, tail });
        if !ok {
            pass = false;
            break;
        }
    }
    GateReport { results, pass }
}

/// TRANSFORM (v1.1): deterministic machine step. `input` goes to stdin,
/// stdout is the artifact text. Non-zero exit or timeout is Err with the
/// stderr tail — the caller parks (no retry: same input, same output).
pub fn run_transform(
    cmd: &str,
    cwd: &Path,
    input: &str,
    timeout_s: u64,
    run_dir: &Path,
) -> Result<String, String> {
    crate::status::emit(run_dir, |st| {
        st.state = "transform".into();
        st.detail = cmd.to_string();
    });
    let out_path = std::env::temp_dir().join(format!("workflow-transform-{}.out", std::process::id()));
    let err_path = std::env::temp_dir().join(format!("workflow-transform-{}.err", std::process::id()));
    let make = |p: &Path| std::fs::File::create(p).map_err(|e| format!("transform log create failed: {e}"));
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .env("WORKFLOW_RUN_DIR", run_dir)
        .stdin(Stdio::piped())
        .stdout(make(&out_path)?)
        .stderr(make(&err_path)?)
        .spawn()
        .map_err(|e| format!("transform spawn failed: {e}"))?;
    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        use std::io::Write as _;
        // The command may exit without reading stdin — a broken pipe is fine.
        let _ = stdin.write_all(input.as_bytes());
    }
    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_s);
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let tail = read_tail(&err_path);
                cleanup(&[&out_path, &err_path]);
                return Err(format!("transform TIMEOUT after {timeout_s}s (killed)\n{tail}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                cleanup(&[&out_path, &err_path]);
                return Err(format!("transform wait failed: {e}"));
            }
        }
    };
    let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
    let tail = read_tail(&err_path);
    cleanup(&[&out_path, &err_path]);
    if exit != 0 {
        return Err(format!("transform exited {exit}:\n{tail}"));
    }
    Ok(stdout)
}

fn cleanup(paths: &[&Path]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// D8: record a completed verdict step in the shared decisions trail via
/// `scripts/gate_runner.py review` — the trail has ONE home, this module is
/// its only caller. Failure is a hard run error, never silent.
pub fn record_review(
    repo_root: &Path,
    task: &str,
    verdict: &str,
    subject: &str,
    rationale: &str,
    by: &str,
) -> Result<(), String> {
    let out = Command::new(repo_root.join("scripts/gate_runner.py"))
        .args(["review", "--task", task, "--verdict", verdict, "--subject", subject, "--rationale", rationale, "--by", by])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("gate_runner review spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gate_runner review failed (verdict for {subject} is NOT on the record):\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn run_one(
    cmd: &str,
    cwd: &Path,
    timeout: Duration,
    run_dir: &Path,
    extra_env: &[(&str, &Path)],
) -> (i32, String) {
    let tmp = std::env::temp_dir().join(format!("workflow-gate-{}.log", std::process::id()));
    let log = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => return (-1, format!("gate log create failed: {e}")),
    };
    let err = match log.try_clone() {
        Ok(f) => f,
        Err(e) => return (-1, format!("gate log clone failed: {e}")),
    };
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        // Gate lines reference run state portably via $WORKFLOW_RUN_DIR.
        .env("WORKFLOW_RUN_DIR", run_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err);
    for (k, v) in extra_env {
        command.env(k, v);
    }
    let mut child = match command.spawn()
    {
        Ok(c) => c,
        Err(e) => return (-1, format!("spawn failed: {e}")),
    };
    let started = Instant::now();
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(-1),
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let tail = read_tail(&tmp);
                let _ = std::fs::remove_file(&tmp);
                return (-2, format!("TIMEOUT after {}s (killed)\n{tail}", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => break {
                let _ = child.kill();
                eprintln!("workflow: gate wait failed: {e}");
                -1
            },
        }
    };
    let tail = read_tail(&tmp);
    let _ = std::fs::remove_file(&tmp);
    (exit, tail)
}

fn read_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(TAIL_LINES);
    lines[start..].join("\n")
}
