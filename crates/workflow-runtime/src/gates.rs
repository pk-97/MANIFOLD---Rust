//! Gate execution: shell out, capture exit + tail. Heavy gates and the verdict
//! trail belong to `scripts/gate_runner.py` (WORKFLOW_RUNTIME_DESIGN.md D3) —
//! a program's gate line calls it; this module never re-implements it.
//!
//! INVARIANT (structural, rg-gated at landing): this is the only module in the
//! crate that spawns a subprocess.

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
    let mut results = Vec::new();
    let mut pass = true;
    for cmd in cmds {
        let (exit, tail) = run_one(cmd, cwd, Duration::from_secs(timeout_s), run_dir);
        let ok = exit == 0;
        results.push(GateResult { cmd: cmd.clone(), exit, tail });
        if !ok {
            pass = false;
            break;
        }
    }
    GateReport { results, pass }
}

fn run_one(cmd: &str, cwd: &Path, timeout: Duration, run_dir: &Path) -> (i32, String) {
    let tmp = std::env::temp_dir().join(format!("workflow-gate-{}.log", std::process::id()));
    let log = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => return (-1, format!("gate log create failed: {e}")),
    };
    let err = match log.try_clone() {
        Ok(f) => f,
        Err(e) => return (-1, format!("gate log clone failed: {e}")),
    };
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        // Gate lines reference run state portably via $WORKFLOW_RUN_DIR.
        .env("WORKFLOW_RUN_DIR", run_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()
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
