//! God-file regrowth ratchet. `godfile_ceilings.json` holds the line `limit` and a
//! ceiling for every .rs file under `crates/` over the limit at its current size,
//! plus files a design doc caps lower and the split hubs. A listed file may not
//! exceed its ceiling; an unlisted file may not exceed the limit. Lowering a ceiling
//! is always fine; raising one is a reviewed edit to the JSON.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct Ceilings {
    limit: usize,
    ceilings: BTreeMap<String, usize>,
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() != "target" {
                walk(&path, root, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, bytes.iter().filter(|b| **b == b'\n').count()));
        }
    }
}

#[test]
fn no_file_regrows_past_its_ceiling() {
    let table: Ceilings = serde_json::from_str(include_str!("godfile_ceilings.json"))
        .expect("godfile_ceilings.json is not valid");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    walk(&root.join("crates"), &root, &mut files);
    let mut violations = Vec::new();
    for (rel, lines) in &files {
        let ceiling = table.ceilings.get(rel).copied().unwrap_or(table.limit);
        if *lines > ceiling {
            violations.push(format!("{rel}: {lines} lines > ceiling {ceiling}"));
        }
    }
    for rel in table.ceilings.keys() {
        if !files.iter().any(|(f, _)| f == rel) {
            violations.push(format!(
                "{rel}: listed in godfile_ceilings.json but missing; delete the entry"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "god-file regrowth detected (decompose the file; do not bump a ceiling without a \
         design update naming the reason):\n{}",
        violations.join("\n")
    );
}
