//! INV-7 (one text measurer, cached), `docs/UI_INVALIDATION_DESIGN.md`
//! section 6. Outside test code the only `TextMeasure` impls are
//! `CachedTextMeasure` and `HeuristicTextMeasure`, and CoreText is measured
//! (`measure_text_ct`) only from the cache-miss path. Test modules (from a
//! `#[cfg(test)]` that opens a `mod`) and paths containing "test" are skipped,
//! matching `rg 'impl TextMeasure for' crates/ -g '!*test*'`.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("readable dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.contains("test") || name == "target" {
            continue;
        }
        if path.is_dir() {
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

/// The source before its first `#[cfg(test)]` line that is followed by a
/// `mod` line.
fn non_test_source(src: &str) -> &str {
    let mut offset = 0;
    let mut lines = src.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "#[cfg(test)]"
            && lines.peek().is_some_and(|next| next.trim_start().starts_with("mod "))
        {
            return &src[..offset];
        }
        offset += line.len();
    }
    src
}

#[test]
fn one_text_measurer_outside_tests() {
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    assert!(files.len() > 100, "scan found only {} files under {crates:?}", files.len());

    let mut impls = Vec::new();
    let mut ct_calls = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path).expect("readable source");
        let src = non_test_source(&src);
        for line in src.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("impl TextMeasure for ") {
                let ty = rest.split(|c: char| !c.is_alphanumeric() && c != '_').next().unwrap_or("");
                impls.push(ty.to_string());
            }
            if line.contains("measure_text_ct(") && !line.contains("fn measure_text_ct(") {
                ct_calls.push(format!("{}: {}", path.display(), line.trim()));
            }
        }
    }
    impls.sort();
    assert_eq!(impls, ["CachedTextMeasure", "HeuristicTextMeasure"], "TextMeasure impls outside tests");
    assert_eq!(ct_calls.len(), 1, "measure_text_ct called outside the cache-miss path: {ct_calls:#?}");
    assert!(ct_calls[0].contains("cache.fonts"), "the one call is the miss path: {ct_calls:?}");
}
