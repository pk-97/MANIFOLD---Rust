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
    /// waiting-on-lane | gate | transform | retrying | transport-error |
    /// parked | escalated | blocked | abandoned | run-done.
    pub state: String,
    /// Human detail for the state (gate command, prompt size, park reason…).
    pub detail: String,
    pub step: String,
    /// The step's human-readable `title` from the program, when given.
    #[serde(default)]
    pub title: String,
    /// 1-based position of the current step.
    pub step_index: usize,
    pub total_steps: usize,
    pub opcode: String,
    pub model: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub tokens_spent: u64,
    pub token_budget: u64,
    /// Lane spend in dollars — its own field because the lane envelope reports
    /// USD and folding it into a token count would be a lie.
    #[serde(default)]
    pub usd_spent: f64,
    /// Most recent error text (transport or parse), kept across transitions
    /// until the step completes — the first thing `watch` shows loudly.
    pub last_error: String,
}

/// `84120` → `84,120`. Token and size counts are read by eye mid-run.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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
    let retry =
        if st.attempt > 1 { format!(" (attempt {}/{})", st.attempt, st.max_attempts) } else { String::new() };
    println!("workflow: {step}{} {}{retry}", st.state, st.detail);
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
