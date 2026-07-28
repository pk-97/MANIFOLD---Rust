//! Lifecycle gate for shipped design docs (the docs-pile class fix).
//!
//! A SHIPPED design doc must either be cited by a live surface (CLAUDE.md,
//! hooks, memory, any non-shipped doc) or live in `docs/archive/`. The
//! classifier is `.claude/hooks/design_status.py --lifecycle-check` — one
//! classifier, shared with the status board, so this test cannot drift from
//! what the board shows. Fix is always: `git mv` the doc to archive (+ regen
//! the index) or add a `Lifecycle: contract — <why>` header line.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn shipped_docs_are_cited_or_archived() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = repo.join(".claude/hooks/design_status.py");
    let out = Command::new("python3")
        .arg(&script)
        .arg("--lifecycle-check")
        .current_dir(&repo)
        .output()
        .expect("python3 should run design_status.py");
    assert!(
        out.status.success(),
        "docs lifecycle check failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
