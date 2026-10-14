//! Drift guard for repo paths cited in the live docs.
//!
//! Every `scripts/…`, `.claude/…`, `docs/…` or `crates/…` path a live doc
//! cites must exist. Live means `CLAUDE.md` and `docs/**/*.md` outside the
//! frozen records: `docs/archive/`, `docs/landings/` and `docs/BUG_BACKLOG.md`.
//!
//! Only file paths (last component has an extension) and directory paths
//! written with a trailing `/` are checked, so prose like "docs/comments" is
//! not a citation. Globs and placeholders are skipped: a path followed by
//! `*`, `<`, `{`, `$`, `[` or `…`, one ending in `-` or `_`, and one whose
//! file stem is a single character (`scripts/x.py`). Gitignored paths
//! (runtime state such as `.claude/worktrees/`) count as existing.
//!
//! The backlog found when the guard was added lives in
//! `tests/fixtures/docs_cited_paths_baseline.txt`, mostly planned files in
//! approved-not-built designs and godfiles that became directory modules. The
//! test fails only on a dead path not in that list. Fix a failure by pointing
//! the doc at the current path or deleting the citation; never by adding to
//! the baseline. Entries that no longer hit are printed so the list burns down.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PREFIXES: [&str; 4] = ["scripts/", ".claude/", "docs/", "crates/"];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')
}

fn live_docs(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, docs_root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(docs_root).unwrap_or(&p);
            if rel == Path::new("archive")
                || rel == Path::new("landings")
                || rel == Path::new("BUG_BACKLOG.md")
            {
                continue;
            }
            if p.is_dir() {
                walk(&p, docs_root, out);
            } else if p.extension().is_some_and(|x| x == "md") {
                out.push(p);
            }
        }
    }
    let docs = root.join("docs");
    let mut out = vec![root.join("CLAUDE.md")];
    walk(&docs, &docs, &mut out);
    out.sort();
    out
}

/// Repo paths cited in `text` that the guard checks (see module doc).
fn cited_paths(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !(is_path_char(chars[i - 1]) || chars[i - 1] == '~');
        if !boundary || !is_path_char(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_path_char(chars[i]) {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        if !PREFIXES.iter().any(|p| token.starts_with(p)) {
            continue;
        }
        if chars
            .get(i)
            .is_some_and(|c| matches!(c, '*' | '<' | '{' | '$' | '[' | '…'))
        {
            continue;
        }
        let token = token.trim_end_matches(['.', ',', ':', ';']);
        if token.ends_with(['-', '_']) || token.contains("..") {
            continue;
        }
        let last = token.rsplit('/').next().unwrap_or("");
        let is_dir = token.ends_with('/');
        let is_file = match last.rsplit_once('.') {
            Some((stem, ext)) => !stem.is_empty() && !ext.is_empty(),
            None => false,
        };
        if !(is_dir || is_file) {
            continue;
        }
        if is_file && last.rsplit_once('.').is_some_and(|(stem, _)| stem.chars().count() <= 1) {
            continue;
        }
        out.push(token.to_string());
    }
    out
}

/// The subset of `paths` git ignores. Empty when git is unavailable.
fn gitignored(root: &Path, paths: &BTreeSet<String>) -> BTreeSet<String> {
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    else {
        return BTreeSet::new();
    };
    let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let Ok(out) = child.wait_with_output() else { return BTreeSet::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn live_docs_cite_only_existing_repo_paths() {
    let root = repo_root();
    let mut dead: BTreeSet<(String, String)> = BTreeSet::new();
    for doc in live_docs(&root) {
        let text = fs::read_to_string(&doc).unwrap_or_default();
        let rel = doc
            .strip_prefix(&root)
            .unwrap_or(&doc)
            .to_string_lossy()
            .into_owned();
        for path in cited_paths(&text) {
            if !root.join(&path).exists() {
                dead.insert((rel.clone(), path));
            }
        }
    }
    let missing: BTreeSet<String> = dead.iter().map(|(_, p)| p.clone()).collect();
    let ignored = gitignored(&root, &missing);
    let hits: BTreeSet<String> = dead
        .into_iter()
        .filter(|(_, p)| !ignored.contains(p.trim_end_matches('/')) && !ignored.contains(p))
        .map(|(d, p)| format!("{d}:{p}"))
        .collect();

    let baseline_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs_cited_paths_baseline.txt");
    let baseline: BTreeSet<String> = fs::read_to_string(&baseline_path)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();

    let cleared: Vec<&String> = baseline.difference(&hits).collect();
    if !cleared.is_empty() {
        eprintln!(
            "docs_cited_paths: {} baseline entries no longer hit; delete them from {}:",
            cleared.len(),
            baseline_path.display()
        );
        for c in &cleared {
            eprintln!("  {c}");
        }
    }

    let new: Vec<&String> = hits.difference(&baseline).collect();
    assert!(
        new.is_empty(),
        "{} live doc(s) cite repo paths that don't exist. Point each at the current \
         path or delete the citation (the baseline is a burn-down list, not a waiver):\n{}",
        new.len(),
        new.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn cited_paths_skips_globs_placeholders_and_prose() {
    let text = "see `scripts/ui-flows/*.json`, `scripts/x.py`, docs/comments, \
                `docs/landings/2026-07-12-batch-`, `crates/<crate>/src`, \
                ~/.claude/hooks/token-guard.py, /abs/docs/FOO.md, \
                `scripts/dev.py`, (docs/AGENT_ROUTING.md). and scripts/ui-flows/";
    assert_eq!(
        cited_paths(text),
        vec!["scripts/dev.py", "docs/AGENT_ROUTING.md", "scripts/ui-flows/"]
    );
}
