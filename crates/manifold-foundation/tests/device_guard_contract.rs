//! Every real-device opener in the workspace asks `device_guard` first, and only
//! the live GUI opens the gate. See `src/device_guard.rs`.
//!
//! An opener is a line that calls one of `OPENERS`. Its enclosing function must
//! call `device_guard::` before that line. Test code is skipped: `tests/`
//! directories, `*tests.rs` files, and everything after a `#[cfg(test)]` module
//! header. The gate-opening check scans test code too.

use std::path::{Path, PathBuf};

/// Calls that reach real hardware or a machine-shared resource.
const OPENERS: &[&str] = &[
    "build_input_stream(",
    "build_output_stream(",
    "(syms.create)(",
    "requestAccessForMediaType",
    "midir::MidiInput::new(",
    "midir::MidiOutput::new(",
    "AblLink::new(",
    "UdpSocket::bind(",
    "TcpListener::bind(",
    "AudioManager::<DefaultBackend>::new(",
];

/// The only places allowed to open the gate.
const ALLOW_CALLERS: &[&str] = &[
    "crates/manifold-app/src/main.rs",
    "crates/manifold-foundation/src/device_guard.rs",
    "crates/manifold-foundation/tests/device_guard_contract.rs",
];

fn workspace_crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn rust_files(dir: &Path, with_tests: bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if name != "target" && (with_tests || name != "tests") {
                rust_files(&path, with_tests, out);
            }
        } else if name.ends_with(".rs") && (with_tests || !name.ends_with("tests.rs")) {
            out.push(path);
        }
    }
}

/// Production lines of a file: everything before the first `#[cfg(test)]`
/// that heads a module.
fn production_lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    for i in 0..lines.len() {
        if lines[i].trim() == "#[cfg(test)]"
            && lines
                .get(i + 1)
                .is_some_and(|next| next.trim_start().starts_with("mod "))
        {
            return lines[..i].to_vec();
        }
    }
    lines
}

fn is_fn_header(line: &str) -> bool {
    let t = line.trim_start();
    let t = t
        .strip_prefix("pub(crate) ")
        .or_else(|| t.strip_prefix("pub "))
        .unwrap_or(t);
    let t = t.strip_prefix("unsafe ").unwrap_or(t);
    t.starts_with("fn ")
}

fn relative(path: &Path) -> String {
    let root = workspace_crates().parent().unwrap().to_path_buf();
    path.strip_prefix(&root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn every_real_device_opener_asks_the_guard_first() {
    let mut files = Vec::new();
    rust_files(&workspace_crates(), false, &mut files);
    assert!(
        files.len() > 100,
        "scan found too few files ({}); wrong root?",
        files.len()
    );

    let mut seen_openers = 0;
    let mut unguarded = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let lines = production_lines(&text);
        for (i, line) in lines.iter().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || is_fn_header(line) {
                continue;
            }
            let Some(opener) = OPENERS.iter().find(|o| code.contains(*o)) else {
                continue;
            };
            seen_openers += 1;
            let start = (0..i).rev().find(|&j| is_fn_header(lines[j]));
            let guarded = start.is_some_and(|s| {
                lines[s..i].iter().any(|l| {
                    l.contains("device_guard::allowed(") || l.contains("device_guard::require(")
                })
            });
            if !guarded {
                unguarded.push(format!("{}:{} calls {opener}", relative(path), i + 1));
            }
        }
    }
    assert!(
        seen_openers >= 10,
        "only {seen_openers} openers found; the scan is broken"
    );
    assert!(
        unguarded.is_empty(),
        "real-device openers that do not ask manifold_foundation::device_guard first \
         (call device_guard::require or device_guard::allowed earlier in the same fn):\n{}",
        unguarded.join("\n")
    );
}

#[test]
fn only_the_live_app_opens_the_gate() {
    let mut files = Vec::new();
    rust_files(&workspace_crates(), true, &mut files);
    let mut callers = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let rel = relative(path);
        if text.contains("allow_real_devices()") && !ALLOW_CALLERS.contains(&rel.as_str()) {
            callers.push(rel);
        }
    }
    assert!(
        callers.is_empty(),
        "only the live GUI may call device_guard::allow_real_devices(); found in:\n{}",
        callers.join("\n")
    );
}
