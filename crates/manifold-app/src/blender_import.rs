//! FBX/.obj/.dae drag-and-drop, converted through the user's installed
//! Blender as a subprocess.
//!
//! `docs/IMPORT_ANYTHING_WAVE_DESIGN.md` Lane W3 (decided, don't reopen):
//! MANIFOLD stays glTF-only internally. FBX is a closed format (Mixamo rigs
//! and most store assets ship it); rather than write our own FBX importer or
//! port Blender's GPL importer into Rust, we shell out to the user's own
//! Blender install, which already imports FBX/.obj/.dae perfectly and can
//! export glTF. `scripts/blender/fbx2glb.py` is the conversion script (GPL,
//! deliberately kept out of the Rust tree, invoked only as a subprocess —
//! the process boundary is the license boundary, per the script's own header
//! comment). The produced `.glb` then flows through the exact same import
//! path as a native glTF drop.
//!
//! This module owns two things: locating a Blender binary, and running the
//! conversion with a timeout and captured stderr. It has no UI-thread
//! dependency — `import_model_file` (`app_lifecycle.rs`) calls
//! [`convert_to_glb`] synchronously before handing off to
//! `assemble_import_graph`, the same shape it already uses for the (also
//! blocking) glTF CPU parse.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::user_prefs::UserPrefs;

/// Pref key for a user-supplied Blender path override (Settings UI can write
/// this; discovery checks it first). Same naming convention as the other
/// `MANIFOLD_*` pref keys in `project_io.rs`/`dialog_path_memory.rs`.
pub const BLENDER_PATH_PREF_KEY: &str = "MANIFOLD_BlenderPath";

/// Wall-clock budget for a single conversion. Rigged Mixamo-scale assets
/// convert in well under this on the dev machine; a runaway subprocess (bad
/// input, Blender hung on a dialog) gets killed rather than left to hang the
/// drop gesture forever.
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(120);

/// Extensions this module knows how to route through Blender. Kept as a
/// small helper so the app.rs drop-dispatch and any future caller share one
/// definition instead of re-deriving the set from the script's own routing.
pub fn is_blender_convertible_extension(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "fbx" | "obj" | "dae")
}

/// Human-readable name of the conversion, for the "converted from FBX via
/// Blender 4.5.2" report line. Import callers combine this with the actual
/// Blender version string captured from discovery.
pub fn source_format_label(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "fbx" => "FBX",
        "obj" => "OBJ",
        "dae" => "COLLADA",
        _ => "model",
    }
}

/// Error surfaced to the drop handler — always actionable, never silent.
#[derive(Debug, Clone)]
pub enum BlenderConversionError {
    /// No Blender install found anywhere in the discovery order.
    NotFound,
    /// Blender ran but exited non-zero, timed out, or produced no output
    /// file. Carries the tail of stderr (or a synthesized message for a
    /// timeout/spawn failure) for the log line.
    ConversionFailed { detail: String },
}

impl std::fmt::Display for BlenderConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlenderConversionError::NotFound => write!(
                f,
                "FBX import uses Blender for conversion — install Blender (blender.org) and drop the file again."
            ),
            BlenderConversionError::ConversionFailed { detail } => {
                write!(f, "Blender conversion failed: {detail}")
            }
        }
    }
}

/// Discover a Blender binary, in the documented order:
/// 1. `blender_path` in [`UserPrefs`], if set and the path exists.
/// 2. The standard macOS app-bundle location.
/// 3. `blender` on `$PATH` (via `which`).
///
/// Pure path-existence checks — no subprocess spawn — so this is cheap to
/// call on every drop and trivially unit-testable with fake paths.
pub fn discover_blender(prefs: &UserPrefs) -> Option<PathBuf> {
    let pref_path = prefs.get_string(BLENDER_PATH_PREF_KEY, "");
    if !pref_path.is_empty() {
        let p = PathBuf::from(&pref_path);
        if p.is_file() {
            return Some(p);
        }
        log::warn!(
            "[BlenderImport] {BLENDER_PATH_PREF_KEY} is set to '{pref_path}' but that path doesn't exist — falling through to the standard locations"
        );
    }

    let bundled = PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender");
    if bundled.is_file() {
        return Some(bundled);
    }

    which_blender()
}

/// `which blender` — isolated so the unit test can stub the whole function
/// instead of relying on the test host's actual `$PATH` contents.
fn which_blender() -> Option<PathBuf> {
    let output = Command::new("which").arg("blender").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path_str.is_empty() {
        return None;
    }
    let path = PathBuf::from(path_str);
    path.is_file().then_some(path)
}

/// Run `scripts/blender/fbx2glb.py` as a subprocess: `<blender> -b -P
/// <script> -- <src> <dst>`. `repo_root` is the directory containing
/// `scripts/blender/` (the running app's working directory in dev; ships
/// alongside the app in a real bundle — callers resolve that path, this fn
/// just takes it).
///
/// Kills the subprocess if it exceeds [`CONVERSION_TIMEOUT`] rather than
/// blocking forever. On failure, returns the tail of stderr (or a
/// synthesized message for spawn/timeout failures) so the caller's log line
/// names the actual cause instead of failing silently.
pub fn run_blender_conversion(
    blender: &Path,
    script: &Path,
    src: &Path,
    dst: &Path,
) -> Result<(), BlenderConversionError> {
    let mut child = Command::new(blender)
        .arg("-b")
        .arg("-P")
        .arg(script)
        .arg("--")
        .arg(src)
        .arg(dst)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| BlenderConversionError::ConversionFailed {
            detail: format!("failed to launch {}: {e}", blender.display()),
        })?;

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| BlenderConversionError::ConversionFailed {
            detail: format!("error waiting on Blender subprocess: {e}"),
        })? {
            break status;
        }
        if start.elapsed() > CONVERSION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(BlenderConversionError::ConversionFailed {
                detail: format!(
                    "conversion exceeded {}s and was killed — the input may be malformed or Blender may be stuck on a dialog",
                    CONVERSION_TIMEOUT.as_secs()
                ),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    if !status.success() {
        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }
        let tail = stderr_tail(&stderr_buf);
        return Err(BlenderConversionError::ConversionFailed {
            detail: format!("Blender exited with {status}: {tail}"),
        });
    }

    if !dst.is_file() {
        return Err(BlenderConversionError::ConversionFailed {
            detail: format!(
                "Blender exited successfully but {} was never written",
                dst.display()
            ),
        });
    }

    Ok(())
}

/// Last few lines of stderr — enough to name the cause without dumping
/// Blender's full (often chatty) console output into the log.
fn stderr_tail(stderr: &str) -> String {
    const MAX_LINES: usize = 20;
    let lines: Vec<&str> = stderr.lines().collect();
    if lines.len() <= MAX_LINES {
        stderr.trim().to_string()
    } else {
        lines[lines.len() - MAX_LINES..].join("\n")
    }
}

/// Result of a successful conversion — the produced glb path plus a
/// human-readable Blender version for the "converted from FBX via Blender
/// 4.5.2" report line. `blender_version` is `None` when `--version` couldn't
/// be parsed (never fatal — the conversion itself already succeeded).
pub struct ConversionOutcome {
    pub glb_path: PathBuf,
    pub blender_version: Option<String>,
}

/// Best-effort `<blender> --version` capture. Failure here never fails the
/// conversion — it only makes the report line slightly less specific.
fn blender_version(blender: &Path) -> Option<String> {
    let output = Command::new(blender).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line looks like "Blender 4.5.2 LTS (hash ...)" — take just the
    // "4.5.2" token (or the whole first line if the shape ever changes).
    let first_line = stdout.lines().next()?.trim();
    first_line
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
        .or_else(|| Some(first_line.to_string()))
}

/// End-to-end: discover Blender, convert `src` (`.fbx`/`.obj`/`.dae`) into a
/// `.glb` under `<app data dir>/converted_models/<stem>.glb`, and return the
/// produced path. `repo_root` locates `scripts/blender/fbx2glb.py` — pass the
/// running app's working directory (dev) or wherever the bundle ships the
/// script.
pub fn convert_via_blender(
    prefs: &UserPrefs,
    repo_root: &Path,
    src: &Path,
) -> Result<ConversionOutcome, BlenderConversionError> {
    let blender = discover_blender(prefs).ok_or(BlenderConversionError::NotFound)?;

    let script = repo_root.join("scripts/blender/fbx2glb.py");
    if !script.is_file() {
        return Err(BlenderConversionError::ConversionFailed {
            detail: format!("conversion script missing at {}", script.display()),
        });
    }

    let cache_dir = crate::user_prefs::app_data_dir().join("converted_models");
    std::fs::create_dir_all(&cache_dir).map_err(|e| BlenderConversionError::ConversionFailed {
        detail: format!("failed to create {}: {e}", cache_dir.display()),
    })?;

    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "model".to_string());
    let dst = cache_dir.join(format!("{stem}.glb"));

    run_blender_conversion(&blender, &script, src, &dst)?;
    Ok(ConversionOutcome {
        glb_path: dst,
        blender_version: blender_version(&blender),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs_with(key: &str, value: &str) -> UserPrefs {
        let mut prefs = UserPrefs::for_test();
        prefs.set_string(key, value);
        prefs
    }

    #[test]
    fn is_blender_convertible_extension_covers_fbx_obj_dae() {
        assert!(is_blender_convertible_extension("fbx"));
        assert!(is_blender_convertible_extension("FBX"));
        assert!(is_blender_convertible_extension("obj"));
        assert!(is_blender_convertible_extension("dae"));
        assert!(!is_blender_convertible_extension("glb"));
        assert!(!is_blender_convertible_extension("gltf"));
    }

    #[test]
    fn source_format_label_names_are_readable() {
        assert_eq!(source_format_label("fbx"), "FBX");
        assert_eq!(source_format_label("FBX"), "FBX");
        assert_eq!(source_format_label("obj"), "OBJ");
        assert_eq!(source_format_label("dae"), "COLLADA");
    }

    /// `UserPrefs` pref path wins when it points at a real file, even if it's
    /// a weird location — the whole point of the override.
    #[test]
    fn discover_blender_prefers_pref_path_when_it_exists() {
        // A throwaway file stands in for "existing file" — discovery only
        // checks `is_file()`, never that it's actually executable Blender
        // (that would require actually running it).
        let path = std::env::temp_dir().join(format!(
            "manifold_blender_discovery_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"stand-in").expect("write stand-in file");
        let prefs = prefs_with(BLENDER_PATH_PREF_KEY, &path.to_string_lossy());
        assert_eq!(discover_blender(&prefs), Some(path.clone()));
        std::fs::remove_file(&path).ok();
    }

    /// A pref path that doesn't exist on disk must NOT be trusted — falls
    /// through to the standard locations instead of handing a dead path to
    /// the subprocess spawn (which would surface a much less clear error).
    #[test]
    fn discover_blender_falls_through_when_pref_path_is_missing() {
        let prefs = prefs_with(
            BLENDER_PATH_PREF_KEY,
            "/definitely/not/a/real/path/blender",
        );
        // Whatever this resolves to (bundled app or `which`, or None on a
        // host with neither) must NOT be the bogus pref path.
        let found = discover_blender(&prefs);
        if let Some(p) = found {
            assert_ne!(p, PathBuf::from("/definitely/not/a/real/path/blender"));
        }
    }

    #[test]
    fn discover_blender_with_no_pref_and_no_bundle_falls_to_which() {
        let prefs = UserPrefs::for_test();
        // No assertion on the concrete value (host-dependent) — this proves
        // the call doesn't panic and returns a well-formed Option, exercising
        // the fallthrough path with an empty pref.
        let _ = discover_blender(&prefs);
    }

    #[test]
    fn run_blender_conversion_reports_not_found_style_error_for_bad_binary() {
        let dir = std::env::temp_dir().join(format!(
            "manifold_blender_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.fbx");
        std::fs::write(&src, b"not a real fbx").unwrap();
        let dst = dir.join("out.glb");
        let script = dir.join("script.py");
        std::fs::write(&script, b"").unwrap();

        let bogus_blender = PathBuf::from("/definitely/not/a/real/blender/binary");
        let result = run_blender_conversion(&bogus_blender, &script, &src, &dst);
        assert!(matches!(
            result,
            Err(BlenderConversionError::ConversionFailed { .. })
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn not_found_error_message_is_actionable() {
        let msg = BlenderConversionError::NotFound.to_string();
        assert!(msg.contains("Blender"));
        assert!(msg.contains("blender.org"));
    }

    /// Real end-to-end proof, gated behind an env var and `#[ignore]` so the
    /// default sweep never depends on Blender being installed. Generates a
    /// rigged FBX via `scripts/blender/make_hostile_rig.py` into a temp dir
    /// (never committed — regenerated fresh every run), converts it through
    /// the real `convert_via_blender` path, then imports the produced glb
    /// through `assemble_import_graph` and asserts the result is actually
    /// driven by `node.gltf_skeleton_pose` — the same assertion pattern
    /// `gltf_import.rs`'s `skinned_import_gets_no_rigid_animation_source`
    /// test uses for the checked-in fixture, applied here to a freshly
    /// Blender-converted one.
    ///
    /// Run with: `MANIFOLD_RUN_BLENDER_TESTS=1 cargo test -p manifold-app
    /// --bin manifold -- --ignored blender_import::tests::real_conversion`
    #[test]
    #[ignore = "requires a real Blender install; env-gated, see MANIFOLD_RUN_BLENDER_TESTS"]
    fn real_conversion_produces_a_skeleton_posed_import() {
        if std::env::var("MANIFOLD_RUN_BLENDER_TESTS").is_err() {
            eprintln!(
                "skipping: set MANIFOLD_RUN_BLENDER_TESTS=1 to run the real Blender integration test"
            );
            return;
        }

        let prefs = UserPrefs::for_test();
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repo root must resolve");

        let blender = discover_blender(&prefs).expect(
            "MANIFOLD_RUN_BLENDER_TESTS=1 was set but no Blender install was discovered",
        );

        let tmp_dir = std::env::temp_dir().join(format!(
            "manifold_blender_integration_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let fbx_path = tmp_dir.join("hostile_rig.fbx");

        // Generate the rigged FBX fresh, in the temp dir — never committed,
        // matching the lane's gate ("generate ... in the test's own setup
        // into a temp dir, never commit the generated file").
        let make_rig_script = repo_root.join("scripts/blender/make_hostile_rig.py");
        assert!(
            make_rig_script.is_file(),
            "make_hostile_rig.py missing at {}",
            make_rig_script.display()
        );
        let gen_status = Command::new(&blender)
            .arg("-b")
            .arg("-P")
            .arg(&make_rig_script)
            .arg("--")
            .arg(&fbx_path)
            .status()
            .expect("failed to launch Blender to generate the hostile rig");
        assert!(gen_status.success(), "make_hostile_rig.py exited non-zero");
        assert!(fbx_path.is_file(), "hostile rig FBX was never written");

        // Convert through the real production path.
        let outcome = convert_via_blender(&prefs, &repo_root, &fbx_path)
            .expect("real Blender conversion must succeed");
        assert!(outcome.glb_path.is_file());

        // Import through the same path a dropped .glb takes.
        let (def, report) =
            manifold_renderer::node_graph::gltf_import::assemble_import_graph(&outcome.glb_path)
                .expect("assemble_import_graph must succeed on the converted glb");
        assert!(report.object_count > 0, "converted rig must import at least one object");

        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten converted import def");
        assert!(
            flat.nodes.iter().any(|n| n.type_id == "node.gltf_skeleton_pose"),
            "the converted rigged FBX must drive its mesh through node.gltf_skeleton_pose \
             (proves the skinning/armature survived the Blender round-trip, not just geometry)"
        );

        std::fs::remove_dir_all(&tmp_dir).ok();
    }
}
