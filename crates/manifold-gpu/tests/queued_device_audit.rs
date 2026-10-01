//! Fails when a workspace test, harness, example or bin makes a `GpuDevice`
//! without the machine-wide GPU queue (`GpuDevice::new_queued`).
//!
//! `GpuDevice::new` is unqueued on purpose: manifold-gpu ships in the live app
//! and the analyzer plugin. So every dev-only caller has to opt in, and this
//! scan is what notices a new one that forgot. Source scan, no GPU needed.
//!
//! The scan lists every `GpuDevice::new(` and `GpuContext::new(` outside
//! comments under `crates/`; `plugins/` is out of scope (shipped product).
//! Allowed callers:
//! - `manifold-gpu/src/`: the definition, and its own unit tests, which take
//!   the queue inside `new` under `cfg(test)`.
//! - `manifold-app/src/app.rs`: the live GUI, which must never queue.
//! - `manifold-renderer/src/gpu.rs`: the `GpuContext` wrapper definition.

use std::path::{Path, PathBuf};

const ALLOWED: &[&str] = &[
    "manifold-gpu/src/",
    "manifold-app/src/app.rs",
    "manifold-renderer/src/gpu.rs",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_dev_path_device_is_queued() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    assert!(files.len() > 100, "scan found {} files; wrong root?", files.len());

    let mut offenders = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|a| rel.starts_with(a) || rel.ends_with(a)) {
            continue;
        }
        if rel == "manifold-gpu/tests/queued_device_audit.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("GpuDevice::new(") || line.contains("GpuContext::new(") {
                offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "unqueued GpuDevice::new() in dev paths; use GpuDevice::new_queued(\"label\"):\n{}",
        offenders.join("\n")
    );
}
