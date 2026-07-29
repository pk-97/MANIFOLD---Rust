//! Deterministic locate: `anchor:` inputs resolve a symbol to its defining
//! span mechanically — ripgrep's own libraries (`ignore` walk + `regex`)
//! in-process, no model call, no binary on PATH (runs must work from cron).
//! A failed or ambiguous resolve is a loud park with the exact miss — never a
//! silent guess. Span-level extraction means a godfile contributes one item,
//! not 5000 lines.
//!
//! Forms: `anchor:Symbol` (whole tree) · `anchor:path/to/file.rs#Symbol`.
//! Rust-shaped definition matching (this repo's language); other languages
//! use `file:` inputs or a `transform` step.

use std::path::{Path, PathBuf};

/// Resolve `spec` (the part after `anchor:`) against `root`. Returns the
/// span with a `// path:start-end` provenance line.
pub fn resolve(root: &Path, spec: &str) -> Result<String, String> {
    let (file_filter, symbol) = match spec.split_once('#') {
        Some((file, sym)) => (Some(file), sym),
        None => (None, spec),
    };
    if symbol.is_empty() || !symbol.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("anchor {spec:?}: symbol must be a plain identifier"));
    }
    // Definition sites only — call sites would make every anchor ambiguous.
    let pattern = format!(
        r#"^\s*(pub(\([a-z: ]*\))?\s+)?(async\s+)?(unsafe\s+)?(extern\s+"[a-zA-Z]*"\s+)?(const\s+)?(fn|struct|enum|trait|type|const|static|mod|union)\s+{symbol}\b|^\s*macro_rules!\s+{symbol}\b"#
    );
    let def = regex::Regex::new(&pattern).map_err(|e| format!("anchor {spec:?}: {e}"))?;
    let files = match file_filter {
        Some(file) => vec![PathBuf::from(file)],
        None => {
            // gitignore-aware walk, sorted for a deterministic hit order.
            let mut files: Vec<PathBuf> = ignore::WalkBuilder::new(root)
                .build()
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
                .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
                .collect();
            files.sort();
            files
        }
    };
    let mut hits: Vec<(String, usize)> = Vec::new();
    for rel in &files {
        let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
            if file_filter.is_some() {
                return Err(format!("anchor {spec:?}: cannot read {}", rel.display()));
            }
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if def.is_match(line) {
                hits.push((rel.display().to_string(), i + 1));
            }
        }
    }
    match hits.as_slice() {
        [] => Err(format!(
            "anchor {spec:?} resolves to nothing — no definition of {symbol:?} found{}",
            file_filter.map(|f| format!(" in {f}")).unwrap_or_default()
        )),
        [(path, line)] => extract_span(root, path, *line),
        many => Err(format!(
            "anchor {spec:?} is ambiguous — {} definitions: {}. Disambiguate with anchor:<path>#{symbol}",
            many.len(),
            many.iter()
                .map(|(p, l)| format!("{p}:{l}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// From the definition line, take the whole item by brace counting; an item
/// that ends with `;` before any `{` (consts, type aliases) is its own span.
/// Naive about braces in strings/comments — deterministic, worst case a
/// slightly long span, never a wrong start.
fn extract_span(root: &Path, path: &str, def_line: usize) -> Result<String, String> {
    let text = std::fs::read_to_string(root.join(path))
        .map_err(|e| format!("anchor target {path}: {e}"))?;
    let lines: Vec<&str> = text.lines().collect();
    if def_line == 0 || def_line > lines.len() {
        return Err(format!("anchor target {path}:{def_line} is out of range"));
    }
    let start = def_line - 1;
    let mut depth: i64 = 0;
    let mut opened = false;
    let mut end = start;
    'scan: for (i, line) in lines.iter().enumerate().skip(start) {
        for c in line.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => {
                    depth -= 1;
                    if opened && depth == 0 {
                        end = i;
                        break 'scan;
                    }
                }
                ';' if !opened => {
                    end = i;
                    break 'scan;
                }
                _ => {}
            }
        }
        end = i;
    }
    let body = lines[start..=end].join("\n");
    Ok(format!("// {path}:{}-{}\n{body}", start + 1, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("workflow-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(
            dir.join("a.rs"),
            "pub fn target_fn(x: u32) -> u32 {\n    let y = x + 1;\n    y\n}\n\npub const TARGET_CONST: u32 = 7;\nfn twin() {}\n",
        )
        .unwrap();
        std::fs::write(dir.join("sub/b.rs"), "fn twin() {}\nfn caller() { target_fn(1); }\n").unwrap();
        dir
    }

    #[test]
    fn resolves_unique_symbol_to_its_span() {
        let root = fixture();
        let span = resolve(&root, "target_fn").unwrap();
        assert!(span.starts_with("// a.rs:1-4"), "{span}");
        assert!(span.contains("let y = x + 1;"));
        assert!(!span.contains("TARGET_CONST"), "span must stop at the item's end: {span}");
    }

    #[test]
    fn const_span_is_the_statement() {
        let root = fixture();
        let span = resolve(&root, "TARGET_CONST").unwrap();
        assert!(span.contains("a.rs:6-6"), "{span}");
    }

    #[test]
    fn zero_and_ambiguous_are_loud() {
        let root = fixture();
        let err = resolve(&root, "does_not_exist").unwrap_err();
        assert!(err.contains("resolves to nothing"), "{err}");
        let err = resolve(&root, "twin").unwrap_err();
        assert!(err.contains("ambiguous") && err.contains("a.rs") && err.contains("sub/b.rs"), "{err}");
        // The file# form disambiguates.
        let span = resolve(&root, "sub/b.rs#twin").unwrap();
        assert!(span.contains("sub/b.rs:1-1"), "{span}");
    }

    #[test]
    fn call_sites_do_not_count_as_definitions() {
        let root = fixture();
        // target_fn is CALLED in sub/b.rs — must still resolve uniquely to a.rs.
        let span = resolve(&root, "target_fn").unwrap();
        assert!(span.contains("a.rs"), "{span}");
    }
}
