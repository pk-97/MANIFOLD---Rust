//! EXECUTE's side effects (WORKFLOW_RUNTIME_DESIGN.md D5): worktree acquisition
//! through the slot ring, change-set application, pathspec-only commits.
//!
//! INVARIANT (structural, rg-gated): subprocess spawns live only here, in
//! `gates.rs`, `lane.rs`, and in `transport.rs`'s keyget.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::artifacts::{ChangeSet, FailureKind};

/// An apply failure, typed so the promotion decision never reads error text.
pub type ApplyError = (FailureKind, String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    /// Slot label when ring-acquired; None for a caller-provided path.
    pub slot: Option<String>,
}

/// Acquire a worktree from the slot ring (`scripts/agent-worktree.py` — the
/// only sanctioned way to make one; raw `git worktree add` is hook-denied).
pub fn acquire(repo_root: &Path, label: &str, branch: &str, tip: Option<&str>) -> Result<Worktree, String> {
    let mut cmd = Command::new(repo_root.join("scripts/agent-worktree.py"));
    cmd.arg("acquire").arg(label).arg(branch).current_dir(repo_root);
    // Name the lease and give it a pid to probe. A run never releases its slot
    // (completion, `abandon`, and a kill all leave the lease), so an anonymous
    // 8h lease was the ring's only clue that the holder was gone — this process
    // outlives the acquire, so its pid is the honest liveness signal.
    cmd.arg("--owner").arg(format!("workflow-run:{label}"));
    cmd.arg("--holder-pid").arg(std::process::id().to_string());
    if let Some(tip) = tip {
        cmd.arg("--tip").arg(tip);
    }
    let out = cmd.output().map_err(|e| format!("agent-worktree.py spawn failed: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return Err(format!("worktree acquire failed (POOL FULL is a loud stop):\n{text}"));
    }
    let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
    // The path may contain spaces (this repo does); take the whole line.
    let path = field("WORKTREE:").ok_or(format!("no WORKTREE: line in acquire output:\n{text}"))?;
    Ok(Worktree {
        path: PathBuf::from(path),
        slot: field("SLOT:").and_then(|s| s.split_whitespace().next()).map(String::from),
    })
}

/// Apply a change set. Edits are exact-match and must be UNIQUE in the file —
/// zero or multiple matches is an error fed back to the model (D5a).
/// Returns the touched paths (the commit pathspec).
///
/// One-shot writes are NEW-FILE-ONLY: a write over an existing file is refused
/// before the tree is touched. That is a spend decision, not a correctness
/// rule — a legitimately small whole-file rewrite now routes to a paid lane,
/// which is the intended trade (D5, amended).
pub fn apply(worktree: &Path, change: &ChangeSet) -> Result<Vec<String>, ApplyError> {
    // A path in both `edits` and `writes` would silently discard the edit
    // (finding 12) — refuse.
    for e in &change.edits {
        if change.writes.iter().any(|wr| wr.path == e.path) {
            return Err((
                FailureKind::RejectedWrite,
                format!(
                    "path {} appears in both `edits` and `writes` — pick one; a write replaces the whole file",
                    e.path
                ),
            ));
        }
    }
    for w in &change.writes {
        if worktree.join(&w.path).exists() {
            return Err((
                FailureKind::RejectedWrite,
                format!(
                    "write to {} refused — one-shot writes create NEW files only; rewriting an existing file is lane work. Use `edits` for a targeted change.",
                    w.path
                ),
            ));
        }
    }
    // Two passes — validate every edit, then write. A failed attempt must
    // leave the tree untouched or the next attempt fights phantom state.
    let mut staged: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for edit in &change.edits {
        let text = match staged.get(&edit.path) {
            Some(t) => t.clone(),
            None => fs::read_to_string(worktree.join(&edit.path))
                .map_err(|e| (FailureKind::FindMiss, format!("edit target {}: {e}", edit.path)))?,
        };
        let snippet: String = edit.find.chars().take(160).collect();
        // Exact match only. A whitespace-tolerant or fuzzy re-anchor can land
        // an edit in the wrong place and still pass the gate — never add one.
        match text.matches(&edit.find).count() {
            0 => {
                return Err((
                    FailureKind::FindMiss,
                    format!(
                        "edit target {}: this `find` text is not in the file (quote the CURRENT file exactly, whitespace included): {snippet:?}",
                        edit.path
                    ),
                ));
            }
            1 => {}
            n => {
                return Err((
                    FailureKind::FindMiss,
                    format!(
                        "edit target {}: `find` matches {n} times — add surrounding lines to make it unique: {snippet:?}",
                        edit.path
                    ),
                ));
            }
        }
        staged.insert(edit.path.clone(), text.replacen(&edit.find, &edit.replace, 1));
    }
    let mut paths = Vec::new();
    let io = |e: std::io::Error| (FailureKind::RejectedWrite, e.to_string());
    for (path, text) in &staged {
        fs::write(worktree.join(path), text).map_err(io)?;
        paths.push(path.clone());
    }
    for w in &change.writes {
        let path = worktree.join(&w.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        fs::write(&path, &w.content).map_err(io)?;
        paths.push(w.path.clone());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Resume guard (finding 9): the ring may have re-issued a slot since this
/// run last touched it. Verify the tree exists and is on OUR branch before
/// committing anything into it.
pub fn verify(wt: &Worktree, expected_branch: Option<&str>) -> Result<(), String> {
    if !wt.path.is_dir() {
        return Err(format!("worktree {} no longer exists — re-run with a fresh run-id", wt.path.display()));
    }
    let Some(expected) = expected_branch else { return Ok(()) };
    let out = Command::new("git")
        .arg("-C")
        .arg(&wt.path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    let actual = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if actual != expected {
        return Err(format!(
            "worktree {} is on branch {actual:?}, expected {expected:?} — the slot was likely re-issued; use a fresh run-id",
            wt.path.display()
        ));
    }
    Ok(())
}

/// Anything a lane left uncommitted. A lane commits its own work by doctrine,
/// so this is only ever read as a PROTOCOL VIOLATION check — never as a
/// pathspec to commit. Committing a porcelain listing is `add -A` with extra
/// steps (untracked scratch, collapsed `dir/` entries), which is a hard rule.
/// `-z` because this repo's paths have spaces and plain porcelain quotes them.
pub fn dirty_paths(worktree: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("git status failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split('\0').filter(|s| !s.is_empty());
    let mut paths = Vec::new();
    while let Some(entry) = fields.next() {
        let (code, path) = entry.split_at(entry.len().min(3));
        // A rename's source is the next NUL field; both sides are dirt, and
        // naming one of them is enough to report the violation.
        if code.starts_with('R') || code.starts_with('C') {
            fields.next();
        }
        if !path.is_empty() {
            paths.push(path.to_string());
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// What a lane actually did: the files its own commits touched between the
/// recorded HEAD and now. The sha delta is the oracle — a well-behaved lane
/// leaves the tree clean, so porcelain would read it as having done nothing.
pub fn changed_since(worktree: &Path, before: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--name-only", &format!("{before}..HEAD")])
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("git diff failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

/// INVARIANT: pathspec-only — never the index, never `add -A`.
pub fn commit(worktree: &Path, paths: &[String], message: &str) -> Result<String, String> {
    let run = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .output()
            .map_err(|e| format!("git spawn failed: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    run(&add)?;
    let mut commit: Vec<&str> = vec!["commit", "-m", message, "--"];
    commit.extend(paths.iter().map(String::as_str));
    run(&commit)?;
    run(&["rev-parse", "HEAD"])
}

/// Current HEAD sha — the gate-first pre-flight path (D19) reports this as
/// `commit` when a seeded rerun's gate is already green and no model call
/// (and so no fresh commit) happened this sample.
pub fn head_sha(worktree: &Path) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("git rev-parse HEAD failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
