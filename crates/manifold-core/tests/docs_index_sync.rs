//! Drift guard for the generated docs index (`docs/README.md`).
//!
//! `docs/README.md` is produced by `scripts/gen_docs_index.py` and must list
//! every active doc in `docs/` (top level; `docs/archive/` is the historical
//! bin and is intentionally not indexed). This test fails the suite if the two
//! fall out of sync — a doc added, removed, or renamed without regenerating the
//! index. The fix is always: `python3 scripts/gen_docs_index.py` and commit.
//!
//! It guards *structure* (which docs are listed), not summary wording — summary
//! text lives in each doc and can be improved freely.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn docs_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/manifold-core
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
}

/// Top-level `*.md` files in docs/, excluding the generated index itself.
fn docs_on_disk() -> BTreeSet<String> {
    fs::read_dir(docs_dir())
        .expect("docs/ should exist")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".md") && n != "README.md")
        .collect()
}

/// Doc links in README.md of the form `](NAME.md)` with no path separator
/// (so `archive/…` and any nested links are ignored).
fn docs_linked_in_index() -> BTreeSet<String> {
    let readme = fs::read_to_string(docs_dir().join("README.md"))
        .expect("docs/README.md should exist — run scripts/gen_docs_index.py");
    let mut out = BTreeSet::new();
    let mut rest = readme.as_str();
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        if let Some(j) = rest.find(')') {
            let target = &rest[..j];
            if target.ends_with(".md") && !target.contains('/') {
                out.insert(target.to_string());
            }
            rest = &rest[j + 1..];
        } else {
            break;
        }
    }
    out
}

#[test]
fn docs_index_is_in_sync_with_docs_dir() {
    let on_disk = docs_on_disk();
    let linked = docs_linked_in_index();

    let missing: Vec<_> = on_disk.difference(&linked).collect();
    let stale: Vec<_> = linked.difference(&on_disk).collect();

    assert!(
        missing.is_empty() && stale.is_empty(),
        "docs/README.md is out of sync with docs/.\n  \
         Missing from the index (run `python3 scripts/gen_docs_index.py`): {missing:?}\n  \
         Listed in the index but not on disk (renamed/removed?): {stale:?}",
    );
}
