//! Gate execution: shell out, capture exit + tail. Heavy gates and the verdict
//! trail belong to `scripts/gate_runner.py` (WORKFLOW_RUNTIME_DESIGN.md D3) —
//! a program's gate line calls it; this module never re-implements it.
//!
//! INVARIANT (structural, rg-gated at landing): this is the only module in the
//! crate that spawns a subprocess.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

const TAIL_LINES: usize = 20;

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

/// Run gate commands in order, stopping at the first failure.
pub fn run_gates(cmds: &[String], cwd: &Path) -> GateReport {
    let mut results = Vec::new();
    let mut pass = true;
    for cmd in cmds {
        let out = Command::new("sh").arg("-c").arg(cmd).current_dir(cwd).output();
        let (exit, tail) = match out {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                let lines: Vec<&str> = text.lines().collect();
                let start = lines.len().saturating_sub(TAIL_LINES);
                (o.status.code().unwrap_or(-1), lines[start..].join("\n"))
            }
            Err(e) => (-1, format!("spawn failed: {e}")),
        };
        let ok = exit == 0;
        results.push(GateResult { cmd: cmd.clone(), exit, tail });
        if !ok {
            pass = false;
            break;
        }
    }
    GateReport { results, pass }
}
