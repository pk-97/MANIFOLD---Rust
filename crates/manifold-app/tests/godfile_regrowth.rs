//! God-file regrowth ratchet. `godfile_ceilings.json` lists every .rs file under
//! `crates/` over LIMIT lines at its current size. A listed file may not exceed its
//! ceiling; an unlisted file may not exceed LIMIT. Lowering a ceiling (or deleting
//! the entry once the file is under LIMIT) is always fine; raising one is a reviewed
//! edit to the JSON.

use std::collections::BTreeMap;
use std::path::Path;

const LIMIT: usize = 2500;
const CEILINGS_JSON: &str = include_str!("godfile_ceilings.json");

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                walk(&path, root, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, text.lines().count()));
        }
    }
}

#[test]
fn no_file_regrows_past_its_ceiling() {
    let ceilings: BTreeMap<String, usize> =
        serde_json::from_str(CEILINGS_JSON).expect("godfile_ceilings.json is not valid");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    walk(&root.join("crates"), &root, &mut files);
    let mut violations = Vec::new();
    for (rel, lines) in &files {
        let ceiling = ceilings.get(rel).copied().unwrap_or(LIMIT);
        if *lines > ceiling {
            violations.push(format!("{rel}: {lines} lines > ceiling {ceiling}"));
        }
    }
    for rel in ceilings.keys() {
        if !files.iter().any(|(f, _)| f == rel) {
            violations.push(format!("{rel}: listed in godfile_ceilings.json but missing; delete the entry"));
        }
    }
    assert!(
        violations.is_empty(),
        "god-file regrowth detected (decompose the file; do not bump a ceiling without a \
         design update naming the reason):\n{}",
        violations.join("\n")
    );
}
