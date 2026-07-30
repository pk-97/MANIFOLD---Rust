//! The decision trail: an append-only `ledger.jsonl` beside the run's
//! artifacts. The run dir recorded every stop but never the thinking, so a
//! resumed run started from a stale status line instead of the decision that
//! unblocked it.

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub ts: u64,
    /// park | unpark | promote | abandon | reopen | run-done.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The human's reasoning — what makes a retry worth running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Machine detail: a park reason, an abandon reason, a promotion outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub fn append(
    run_dir: &Path,
    kind: &str,
    step: Option<&str>,
    note: Option<&str>,
    detail: Option<&str>,
) -> Result<(), String> {
    let entry = Entry {
        ts: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        kind: kind.to_string(),
        step: step.map(String::from),
        note: note.map(String::from),
        detail: detail.map(String::from),
    };
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("ledger.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(f, "{}", serde_json::to_string(&entry).expect("ledger Entry serializes")).map_err(|e| e.to_string())
}

/// Tolerant read: a torn line is skipped, same policy as the transcript. The
/// trail is for humans, and half a line must never brick `watch`.
pub fn read(run_dir: &Path) -> Vec<Entry> {
    let Ok(text) = fs::read_to_string(run_dir.join("ledger.jsonl")) else {
        return Vec::new();
    };
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}
