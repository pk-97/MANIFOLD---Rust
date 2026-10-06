//! Source-level guard for the scene panel's manifest-backed row surface.
//!
//! Section 5b of `WIDGET_TREE_DESIGN.md` makes the shared parameter surface
//! the only owner of manifest rows.  This test is deliberately small and
//! source based: a scene-panel lane adding a private slider or row-id hoard
//! should fail before it can become a second interaction path.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn rust_files(root: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, files)?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn line_hits(source: &str, needle: &str) -> Vec<(usize, String)> {
    source
        .lines()
        .enumerate()
        .filter(|(_, text)| text.contains(needle))
        .map(|(line, text)| (line + 1, text.trim().to_owned()))
        .collect()
}

fn panel_source_files(crate_root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    rust_files(&crate_root.join("src/panels"), &mut files)?;
    files.sort();
    files.dedup();
    Ok(files)
}

fn is_scene_panel_file(path: &Path, crate_root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(crate_root.join("src/panels")) else {
        return false;
    };
    let text = relative.to_string_lossy().replace('\\', "/");
    text.starts_with("scene_setup_") || text.starts_with("scene_setup_panel/")
}

fn is_row_routing_collection(line: &str) -> bool {
    // These names describe the private row plumbing this invariant guards.
    // `outliner_row_ids` is intentionally excluded: it routes the scene
    // chrome's selection rows, which are outside the manifest-backed surface.
    let lower = line.to_ascii_lowercase();
    let names = [
        "slider_ids",
        "row_slider_ids",
        "row_track_ids",
        "param_slider_ids",
        "parameter_slider_ids",
        "param_row_ids",
        "numeric_row_ids",
        "drawer_row_ids",
    ];
    names.iter().any(|name| lower.contains(name)) && line.contains("Vec<")
}

fn sanctioned_slider_owner(path: &Path, crate_root: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(crate_root.join("src/panels")) else {
        return false;
    };
    let text = relative.to_string_lossy().replace('\\', "/");
    text == "drawer.rs"
        || text == "layer_header.rs"
        || text.starts_with("param_card/")
        || text == "param_card.rs"
        || text.starts_with("param_slider_shared/")
        || text == "param_slider_shared.rs"
}

#[test]
fn no_bespoke_row_infra() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = panel_source_files(crate_root).expect("panel source tree must be readable");
    let mut violations = Vec::new();

    // Build the needles in pieces so this test does not match its own source.
    let bitmap_slider_new = ["BitmapSlider", "::new"].concat();
    let bitmap_slider_build = ["BitmapSlider", "::build"].concat();
    let bitmap_slider_api = ["BitmapSlider", "::"].concat();
    let slider_row_method = [".", "slider_row("].concat();
    let slider_row_associated = ["::", "slider_row("].concat();
    let bare_slider_method = [".", "slider()"].concat();
    let bare_slider_associated = ["::", "slider()"].concat();
    let slider_spec = ["SliderSpec", " {"].concat();
    let slider_ids_type = ["SliderNodeIds", " {"].concat();
    let add_slider = ["add", "_slider("].concat();
    let low_level_slider_node = ["UINodeType", "::Slider"].concat();
    let shared_slider_builder = ["build", "_row_slider("].concat();
    let numeric_row_builder = ["build", "_numeric_row("].concat();

    for path in files {
        let source = fs::read_to_string(&path).expect("panel source must be readable");
        let relative = path
            .strip_prefix(crate_root)
            .unwrap_or(&path)
            .display()
            .to_string();

        for (line, text) in line_hits(&source, &bitmap_slider_new) {
            if !sanctioned_slider_owner(&path, crate_root) {
                violations.push(format!(
                    "{relative}:{line}: raw BitmapSlider construction (`{text}`); use ParamSurface/RowHost"
                ));
            }
        }
        for (line, text) in line_hits(&source, &bitmap_slider_build) {
            if !sanctioned_slider_owner(&path, crate_root) {
                violations.push(format!(
                    "{relative}:{line}: raw BitmapSlider construction (`{text}`); use ParamSurface/RowHost"
                ));
            }
        }
        if is_scene_panel_file(&path, crate_root) {
            let low_level_patterns = [
                (&bitmap_slider_api, "raw BitmapSlider API"),
                (&slider_row_method, "scene-panel slider row"),
                (&slider_row_associated, "scene-panel slider row"),
                (&bare_slider_method, "scene-panel bare slider"),
                (&bare_slider_associated, "scene-panel bare slider"),
                (&slider_spec, "direct SliderSpec construction"),
                (&slider_ids_type, "direct SliderNodeIds construction"),
                (&add_slider, "low-level UITree slider construction"),
                (&low_level_slider_node, "low-level slider node construction"),
                (&shared_slider_builder, "direct shared slider construction"),
            ];
            for (needle, kind) in low_level_patterns {
                for (line, text) in line_hits(&source, needle) {
                    violations.push(format!(
                        "{relative}:{line}: {kind} (`{text}`); manifest-backed rows must use ParamSurface/RowHost"
                    ));
                }
            }
            for (line, text) in line_hits(&source, &numeric_row_builder) {
                violations.push(format!(
                    "{relative}:{line}: bespoke numeric row builder (`{text}`); use ParamSurface/RowHost"
                ));
            }
        }
        if !sanctioned_slider_owner(&path, crate_root) {
            for (line, text) in source.lines().enumerate()
                .filter(|(_, text)| is_row_routing_collection(text))
                .map(|(line, text)| (line + 1, text.trim().to_owned())) {
                violations.push(format!(
                    "{relative}:{line}: bespoke row-routing id collection (`{text}`); use ParamSurface/RowHost"
                ));
            }
        }
    }

    violations.sort();
    assert!(
        violations.is_empty(),
        "INV-8 no_bespoke_row_infra failed; manifest-backed scene rows have one sanctioned host:\n{}",
        violations.join("\n")
    );
}
