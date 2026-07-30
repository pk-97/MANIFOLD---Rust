//! Token-free live run state (BUG-h9yj, Peter 2026-07-30 mid-P3): every
//! runner transition rewrites `status.json` in the run dir and prints one
//! stdout line, so a run is observable while it happens — not only at exit.
//! `workflow watch <run-dir>` is a reader over this file plus the run dir's
//! existing state. Emit-only: no new tracking state, and a status failure
//! must never fail a run (all IO errors are swallowed here by design).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Status {
    /// Unix seconds of the last transition.
    pub ts: u64,
    /// Machine-readable state: run-started | starting | waiting-on-model |
    /// gate | transform | retrying | transport-error | parked | escalated |
    /// blocked | run-done.
    pub state: String,
    /// Human detail for the state (gate command, prompt size, park reason…).
    pub detail: String,
    pub step: String,
    /// 1-based position of the current step.
    pub step_index: usize,
    pub total_steps: usize,
    pub opcode: String,
    pub model: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    /// Most recent error text (transport or parse), kept across transitions
    /// until the step completes — the first thing `watch` shows loudly.
    pub last_error: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Read-modify-write of `status.json` (atomic via tmp+rename), then one
/// stdout line. Partial updates merge into the previous state so callers
/// only set what they know (gates.rs knows the command, not the step).
pub fn emit(run_dir: &Path, update: impl FnOnce(&mut Status)) {
    let path = run_dir.join("status.json");
    let mut st: Status = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    update(&mut st);
    st.ts = now();
    let tmp = run_dir.join("status.json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(&st).expect("Status serializes")).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    let step = if st.step.is_empty() {
        String::new()
    } else {
        format!("[{}/{} {}] ", st.step_index, st.total_steps, st.step)
    };
    println!("workflow: {step}{} {}", st.state, st.detail);
}

pub fn read(run_dir: &Path) -> Option<Status> {
    serde_json::from_str(&std::fs::read_to_string(run_dir.join("status.json")).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_merges_partial_updates_and_survives_reread() {
        let dir = std::env::temp_dir().join(format!("wf-status-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        emit(&dir, |s| {
            s.state = "starting".into();
            s.step = "mb-a".into();
            s.step_index = 2;
            s.total_steps = 6;
        });
        emit(&dir, |s| {
            s.state = "gate".into();
            s.detail = "cargo clippy".into();
        });
        let st = read(&dir).expect("status.json readable");
        assert_eq!(st.state, "gate");
        assert_eq!(st.step, "mb-a", "step context must survive a partial update");
        assert!(st.ts > 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
