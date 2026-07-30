//! LANE (WORKFLOW_RUNTIME_DESIGN.md D20): the worker for an output judged by
//! RUNNING it. Same worktree, gate, park and blocking discipline as execute —
//! the difference is that the worker edits the tree itself instead of returning
//! a ChangeSet, so a compiler is in the loop.
//!
//! INVARIANT (structural, rg-gated): subprocess spawns live only here, in
//! `gates.rs`, `worktree.rs`, and `transport.rs`'s keyget. D20 sanctions the
//! tool loop for THIS opcode only; no other module gains one.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

/// One lane invocation. `prompt` is already rendered and already scrubbed.
#[derive(Debug, Clone, Serialize)]
pub struct LaneRequest {
    pub worktree: PathBuf,
    pub prompt: String,
    /// Model id passed through to the worker; None takes the worker's default.
    pub model: Option<String>,
    /// cc-fleet provider positional. The reserved id `claude` runs the local
    /// claude CLI on the user's own login.
    pub provider: String,
    pub max_turns: u32,
    pub timeout_s: u64,
    /// Where the live worker records its job handle while it runs.
    pub run_dir: PathBuf,
}

/// What the runtime needs back. `envelope` is recorded verbatim in the step
/// artifact — the worker's own report, unedited, so a shape change downstream
/// is visible instead of silently dropped.
#[derive(Debug, Clone)]
pub struct LaneOutcome {
    pub ok: bool,
    pub envelope: serde_json::Value,
    /// Dollars, not tokens: the ledger keeps the two apart on purpose.
    pub usd: f64,
    /// Empty when `ok`; otherwise the worker's own failure text.
    pub error: String,
}

/// The mock/live seam, mirroring `ModelTransport` — a test substitutes a double
/// so no test ever launches a real agent session.
pub trait LaneWorker: Sync {
    fn run(&self, req: &LaneRequest) -> Result<LaneOutcome, String>;
}

/// `cc-fleet subagent [provider] --model <id> --prompt-file <p> --json
/// --timeout <d> --max-turns <n>`, cwd = the worktree.
///
/// Under `--json` cc-fleet prints exactly ONE Result envelope on stdout and
/// exits 0 only when `ok` is true. `ok` is the contract, not the exit code: a
/// front-loaded failure (unknown provider, budget cap) still prints a
/// well-formed envelope, so success is `ok && exit 0`.
pub struct CcFleetLane;

impl LaneWorker for CcFleetLane {
    fn run(&self, req: &LaneRequest) -> Result<LaneOutcome, String> {
        // Prompt by file, never argv: it is large and quotes source verbatim.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let prompt_path = std::env::temp_dir().join(format!(
            "workflow-lane-{}-{}.md",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&prompt_path, &req.prompt).map_err(|e| format!("lane prompt write failed: {e}"))?;
        let mut cmd = Command::new("cc-fleet");
        cmd.arg("subagent").arg(&req.provider);
        if let Some(model) = &req.model {
            cmd.args(["--model", model]);
        }
        cmd.arg("--prompt-file")
            .arg(&prompt_path)
            .arg("--json")
            .args(["--timeout", &format!("{}s", req.timeout_s)])
            .args(["--max-turns", &req.max_turns.to_string()])
            .current_dir(&req.worktree)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = cmd.spawn().map_err(|e| {
            let _ = std::fs::remove_file(&prompt_path);
            format!("cc-fleet subagent spawn failed: {e}")
        })?;
        // The handle exists for as long as the worker does: a killed runtime
        // must never leave an unrecorded job still writing the worktree.
        // `started_at` lets `watch` show how long the lane has been running —
        // derived at render time, never rewritten while the worker is up.
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let handle = req.run_dir.join("lane-job.json");
        let _ = std::fs::write(
            &handle,
            serde_json::json!({
                "pid": child.id(), "worktree": req.worktree, "provider": req.provider,
                "started_at": started_at,
            })
            .to_string(),
        );
        let spawned = child.wait_with_output();
        let _ = std::fs::remove_file(&handle);
        let _ = std::fs::remove_file(&prompt_path);
        let out = spawned.map_err(|e| format!("cc-fleet subagent wait failed: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(parse_envelope(&stdout, out.status.code().unwrap_or(-1), &String::from_utf8_lossy(&out.stderr)))
    }
}

/// Defensive by design: the LAST line that parses as a JSON object is the
/// envelope, so a wrapper printing a banner first cannot break the read, and a
/// worker that dies without one becomes a failed attempt rather than a crash.
fn parse_envelope(stdout: &str, exit: i32, stderr: &str) -> LaneOutcome {
    let Some(envelope) = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok().filter(serde_json::Value::is_object))
    else {
        return LaneOutcome {
            ok: false,
            envelope: serde_json::json!({"stdout": stdout, "stderr": stderr, "exit": exit}),
            usd: 0.0,
            error: format!("lane worker printed no JSON envelope (exit {exit}):\n{stdout}\n{stderr}"),
        };
    };
    let ok = envelope["ok"].as_bool().unwrap_or(false) && exit == 0;
    let usd = envelope["total_cost_usd"].as_f64().unwrap_or(0.0);
    let error = if ok {
        String::new()
    } else {
        let code = envelope["error_code"].as_str().unwrap_or("LANE_FAILED");
        let msg = envelope["error_msg"].as_str().unwrap_or("no error_msg in the envelope");
        let suggestion = match envelope["suggestion"].as_str() {
            Some(s) if !s.is_empty() => format!(" ({s})"),
            _ => String::new(),
        };
        format!("lane worker failed (exit {exit}) {code}: {msg}{suggestion}")
    };
    LaneOutcome { ok, envelope, usd, error }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_envelope_carries_cost_and_a_failure_envelope_carries_its_reason() {
        let good = parse_envelope(r#"{"ok":true,"result":"done","total_cost_usd":0.42}"#, 0, "");
        assert!(good.ok);
        assert!((good.usd - 0.42).abs() < f64::EPSILON);

        // Exit 0 with ok=false is possible; `ok` decides, never the exit code.
        let bad = parse_envelope(r#"{"ok":false,"error_code":"UNKNOWN_PROVIDER","error_msg":"nope"}"#, 0, "");
        assert!(!bad.ok);
        assert!(bad.error.contains("UNKNOWN_PROVIDER"), "{}", bad.error);

        let none = parse_envelope("segfault", 139, "boom");
        assert!(!none.ok);
        assert!(none.error.contains("no JSON envelope"), "{}", none.error);
    }
}
