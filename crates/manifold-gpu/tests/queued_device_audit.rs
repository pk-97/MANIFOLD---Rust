//! Fails when a workspace test, harness, example or bin can make a Metal
//! device without the machine-wide GPU queue (`GpuDevice::new_queued`).
//!
//! `GpuDevice::new` is unqueued on purpose: manifold-gpu ships in the live app
//! and the analyzer plugin. So every dev-only caller has to opt in, and this
//! scan is what notices a new one that forgot. Source scan, no GPU needed.
//!
//! Two scans under `crates/`; `plugins/` is out of scope (shipped product).
//!
//! Rust: every `GpuDevice::new(`, `GpuDevice::default(` and `GpuContext::new(`
//! outside comments. Allowed callers:
//! - `manifold-gpu/src/`: the definition, and its own unit tests, which take
//!   the queue inside `new` under `cfg(test)`.
//! - `manifold-app/src/app.rs`: the live GUI, which must never queue.
//! - `manifold-node-engine/src/gpu/context.rs`: the `GpuContext` wrapper definition.
//!
//! Native (`.m`, `.mm`, `.swift`, `.c`): every file that calls
//! `MTLCreateSystemDefaultDevice` or `MTLCopyAllDevices` must be listed in
//! `NATIVE`, and every Rust call of its device-making FFI symbols must come
//! after `admit_native_metal()` in the same function. That call takes the
//! queue in the crate's tests and does nothing in the live app.

use std::path::{Path, PathBuf};

const ALLOWED: &[&str] = &[
    "manifold-gpu/src/",
    "manifold-app/src/app.rs",
    "manifold-node-engine/src/gpu/context.rs",
];

/// (native file, Rust wrapper, FFI symbols that reach a device-making call).
const NATIVE: &[(&str, &str, &[&str])] = &[
    (
        "manifold-media/native/MetalVideoDecoderPlugin.m",
        "manifold-media/src/decoder.rs",
        &["VideoDecoder_CreatePool"],
    ),
    (
        "manifold-media/native/MetalEncoderPlugin.m",
        "manifold-media/src/metal_encoder.rs",
        &[
            "MetalEncoder_IsAvailable",
            "MetalEncoder_IsHDRAvailable",
            "MetalEncoder_Create",
            "MetalEncoder_CreateHDR",
            "MetalEncoder_CreateWithDevice",
            "MetalEncoder_CreateHDRWithDevice",
        ],
    ),
];

const ADMISSION: &str = "admit_native_metal()";

fn source_files(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            source_files(&path, exts, out);
        } else if path
            .extension()
            .is_some_and(|e| exts.iter().any(|x| e == *x))
        {
            out.push(path);
        }
    }
}

fn crates_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rel(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

#[test]
fn every_dev_path_device_is_queued() {
    let crates = crates_root();
    let mut files = Vec::new();
    source_files(&crates, &["rs"], &mut files);
    assert!(files.len() > 100, "scan found {} files; wrong root?", files.len());

    let mut offenders = Vec::new();
    for file in files {
        let rel = rel(&crates, &file);
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
            if line.contains("GpuDevice::new(")
                || line.contains("GpuDevice::default(")
                || line.contains("GpuContext::new(")
            {
                offenders.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "unqueued GpuDevice::new()/default() in dev paths; use GpuDevice::new_queued(\"label\"):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn every_native_metal_device_is_queued() {
    let crates = crates_root();
    let mut files = Vec::new();
    source_files(&crates, &["m", "mm", "swift", "c"], &mut files);

    let mut offenders = Vec::new();
    for file in files {
        let rel = rel(&crates, &file);
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let makes_device = text.lines().any(|l| {
            !l.trim_start().starts_with("//")
                && (l.contains("MTLCreateSystemDefaultDevice") || l.contains("MTLCopyAllDevices"))
        });
        if makes_device && !NATIVE.iter().any(|(native, _, _)| *native == rel) {
            offenders.push(format!(
                "{rel}: makes a Metal device but is not in NATIVE; route its Rust callers through {ADMISSION}"
            ));
        }
    }

    for (native, wrapper, symbols) in NATIVE {
        assert!(crates.join(native).exists(), "NATIVE lists missing file {native}");
        let text = std::fs::read_to_string(crates.join(wrapper))
            .unwrap_or_else(|e| panic!("cannot read {wrapper}: {e}"));
        for symbol in *symbols {
            let call = format!("_ffi::{symbol}(");
            let mut seen = false;
            // Admission seen since the last `fn` line; a call before it is unqueued.
            let mut admitted = false;
            for (n, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("fn ") {
                    admitted = false;
                }
                if code.contains(ADMISSION) {
                    admitted = true;
                }
                if code.contains(&call) {
                    seen = true;
                    if !admitted {
                        offenders.push(format!(
                            "{wrapper}:{}: {symbol} reaches native Metal device creation without {ADMISSION}",
                            n + 1
                        ));
                    }
                }
            }
            if !seen {
                offenders.push(format!("{wrapper}: no call of {symbol}; NATIVE is stale"));
            }
        }
    }
    assert!(offenders.is_empty(), "unqueued native Metal devices:\n{}", offenders.join("\n"));
}
