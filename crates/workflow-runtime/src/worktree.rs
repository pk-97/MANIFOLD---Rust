//! EXECUTE's side effects (WORKFLOW_RUNTIME_DESIGN.md D5): worktree acquisition
//! through the slot ring, change-set application, pathspec-only commits.
//!
//! INVARIANT (structural, rg-gated): subprocess spawns live only here, in
//! `gates.rs`, and in `transport.rs`'s keyget.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::artifacts::ChangeSet;

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
pub fn apply(worktree: &Path, change: &ChangeSet) -> Result<Vec<String>, String> {
    // Two passes — validate every edit, then write. A failed attempt must
    // leave the tree untouched or the next attempt fights phantom state.
    let mut staged: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for edit in &change.edits {
        let text = match staged.get(&edit.path) {
            Some(t) => t.clone(),
            None => fs::read_to_string(worktree.join(&edit.path))
                .map_err(|e| format!("edit target {}: {e}", edit.path))?,
        };
        let snippet: String = edit.find.chars().take(160).collect();
        match text.matches(&edit.find).count() {
            0 => {
                return Err(format!(
                    "edit target {}: this `find` text is not in the file (quote the CURRENT file exactly, whitespace included): {snippet:?}",
                    edit.path
                ));
            }
            1 => {}
            n => {
                return Err(format!(
                    "edit target {}: `find` matches {n} times — add surrounding lines to make it unique: {snippet:?}",
                    edit.path
                ));
            }
        }
        staged.insert(edit.path.clone(), text.replacen(&edit.find, &edit.replace, 1));
    }
    let mut paths = Vec::new();
    for (path, text) in &staged {
        fs::write(worktree.join(path), text).map_err(|e| e.to_string())?;
        paths.push(path.clone());
    }
    for w in &change.writes {
        let path = worktree.join(&w.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, &w.content).map_err(|e| e.to_string())?;
        paths.push(w.path.clone());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
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
