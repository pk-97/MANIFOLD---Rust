//! Standard result-reading verbs for `ui-snap` output — `diff` (compare two
//! tree-dump JSON files), `probe` (sample pixel colors from a PNG), and
//! `crop` (extract a sub-rect PNG). Each is both a standalone subcommand
//! (`cargo xtask ui-snap diff|probe|crop ...`, no GPU, no scene render) and,
//! for `probe`/`crop`, a `--probe`/`--crop` flag usable alongside a normal
//! scene render (applied to the just-written BASE PNG by `mod.rs`). Replaces
//! the ad-hoc scripts agents were writing to answer these three questions —
//! see `docs/HEADLESS_UI_HARNESS.md`.
//!
//! Dependency-light on purpose: `serde_json` for `diff`, `image` for
//! `probe`/`crop` — both already gated behind the `ui-snapshot` feature this
//! whole module lives under.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Tolerance for `rect` float comparison — avoids noise from sub-pixel
/// layout jitter that isn't a real change.
const RECT_EPS: f64 = 0.01;

/// One changed field on a node present in both dumps.
struct FieldChange {
    field: String,
    from: String,
    to: String,
}

/// A short label for a report line: prefers the static component `name`,
/// falls back to `text`, else `"-"`.
fn node_label(n: &Value) -> String {
    if let Some(name) = n.get("name").and_then(Value::as_str) {
        return name.to_string();
    }
    if let Some(text) = n.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    "-".to_string()
}

fn node_id(n: &Value) -> Option<u64> {
    n.get("id").and_then(Value::as_u64)
}

/// Human-readable rendering of a JSON value for a diff line: strings print
/// unquoted, a `rect` array prints as `[x,y,w,h]`, everything else via its
/// compact JSON form.
fn value_repr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => format!("[{}]", arr.iter().map(value_repr).collect::<Vec<_>>().join(",")),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// `rect` equality with epsilon tolerance per component; any shape mismatch
/// (missing/non-array) falls back to exact `Value` equality.
fn rects_equal(a: &Value, b: &Value) -> bool {
    match (a.as_array(), b.as_array()) {
        (Some(av), Some(bv)) if av.len() == bv.len() => av.iter().zip(bv.iter()).all(|(x, y)| {
            let (xf, yf) = (x.as_f64().unwrap_or(f64::NAN), y.as_f64().unwrap_or(f64::NAN));
            (xf - yf).abs() <= RECT_EPS
        }),
        _ => a == b,
    }
}

/// Every field that differs between two same-`id` nodes. Compares the UNION
/// of keys present in either node (excluding only `id`, the match key), not a
/// hardcoded field list — so a field added to `dump::dump_tree_ex` later is
/// diffed automatically instead of silently excluded (a v1 hazard: diff would
/// have exited 0 while the dumps differed). `BTreeSet` keeps the report's
/// field order deterministic.
fn diff_node_fields(a: &Value, b: &Value) -> Vec<FieldChange> {
    let mut fields: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for n in [a, b] {
        if let Some(obj) = n.as_object() {
            fields.extend(obj.keys().map(String::as_str));
        }
    }
    fields.remove("id");
    fields
        .into_iter()
        .filter_map(|field| {
            let av = a.get(field).unwrap_or(&Value::Null);
            let bv = b.get(field).unwrap_or(&Value::Null);
            let equal = if field == "rect" { rects_equal(av, bv) } else { av == bv };
            if equal {
                None
            } else {
                Some(FieldChange {
                    field: field.to_string(),
                    from: value_repr(av),
                    to: value_repr(bv),
                })
            }
        })
        .collect()
}

/// Total hit-target count across every surface in a `custom_surfaces` array.
fn surface_target_count(surfaces: &Value) -> usize {
    surfaces
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|s| s.get("targets").and_then(Value::as_array).map_or(0, Vec::len))
                .sum()
        })
        .unwrap_or(0)
}

/// Node-level diff between two tree dumps (schema: `dump::dump_tree_ex` —
/// top-level `{node_count, nodes: [...], custom_surfaces: [...]}`). Matches
/// nodes by `id`; `custom_surfaces` is compared for raw equality (a one-line
/// report, no per-target detail — enough to keep a hit-target change from
/// passing as "identical"). Returns the report as printable lines (one per
/// added/removed/changed node, a summary line last) plus whether anything
/// differed.
fn diff_trees(a: &Value, b: &Value) -> (Vec<String>, bool) {
    static EMPTY: Vec<Value> = Vec::new();
    let a_nodes = a.get("nodes").and_then(Value::as_array).unwrap_or(&EMPTY);
    let b_nodes = b.get("nodes").and_then(Value::as_array).unwrap_or(&EMPTY);

    let mut a_by_id = std::collections::BTreeMap::new();
    for n in a_nodes {
        if let Some(id) = node_id(n) {
            a_by_id.insert(id, n);
        }
    }
    let mut b_by_id = std::collections::BTreeMap::new();
    for n in b_nodes {
        if let Some(id) = node_id(n) {
            b_by_id.insert(id, n);
        }
    }

    let mut lines = Vec::new();
    let (mut added, mut removed, mut changed, mut unchanged) = (0usize, 0usize, 0usize, 0usize);

    for (&id, &an) in &a_by_id {
        match b_by_id.get(&id) {
            None => {
                removed += 1;
                lines.push(format!("node {id} [{}]: removed", node_label(an)));
            }
            Some(&bn) => {
                let field_changes = diff_node_fields(an, bn);
                if field_changes.is_empty() {
                    unchanged += 1;
                } else {
                    changed += 1;
                    let parts: Vec<String> =
                        field_changes.iter().map(|c| format!("{} {} -> {}", c.field, c.from, c.to)).collect();
                    lines.push(format!("node {id} [{}]: {}", node_label(bn), parts.join("; ")));
                }
            }
        }
    }
    for (&id, &bn) in &b_by_id {
        if !a_by_id.contains_key(&id) {
            added += 1;
            lines.push(format!("node {id} [{}]: added", node_label(bn)));
        }
    }

    // Custom surfaces (graph canvas / timeline clips / automation lanes):
    // raw equality on the whole top-level value — a differing hit-target set
    // must count as a difference, or diff lies by omission on exactly the
    // surfaces `UITree::hit_test` can't see.
    let a_surf = a.get("custom_surfaces").unwrap_or(&Value::Null);
    let b_surf = b.get("custom_surfaces").unwrap_or(&Value::Null);
    let surfaces_differ = a_surf != b_surf;
    if surfaces_differ {
        lines.push(format!(
            "custom_surfaces differ ({} -> {} targets across {} -> {} surfaces)",
            surface_target_count(a_surf),
            surface_target_count(b_surf),
            a_surf.as_array().map_or(0, Vec::len),
            b_surf.as_array().map_or(0, Vec::len),
        ));
    }

    lines.push(format!("{added} added, {removed} removed, {changed} changed, {unchanged} unchanged"));
    (lines, added + removed + changed > 0 || surfaces_differ)
}

/// `cargo xtask ui-snap diff <a.json> <b.json>` — load two tree-dump JSON
/// files, print the node-level diff report to stdout, and exit: `0` if the
/// dumps are identical, `1` if anything differs, `2` on a read/parse error.
pub fn cmd_diff(a_path: &str, b_path: &str) {
    let read = |path: &str| -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("ui-snap diff: read {path}: {e}");
            std::process::exit(2);
        })
    };
    let parse = |path: &str, s: &str| -> Value {
        serde_json::from_str(s).unwrap_or_else(|e| {
            eprintln!("ui-snap diff: parse {path}: {e}");
            std::process::exit(2);
        })
    };
    let a = parse(a_path, &read(a_path));
    let b = parse(b_path, &read(b_path));
    let (lines, differs) = diff_trees(&a, &b);
    for line in &lines {
        println!("{line}");
    }
    std::process::exit(if differs { 1 } else { 0 });
}

/// Parse `"x,y[;x,y...]"` into pixel coordinates. Exits 2 on a malformed spec.
fn parse_coords(spec: &str) -> Vec<(u32, u32)> {
    spec.split(';')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let bad = || {
                eprintln!("ui-snap probe: bad coordinate '{pair}' (expected x,y)");
                std::process::exit(2);
            };
            let mut it = pair.split(',');
            let x: u32 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or_else(bad);
            let y: u32 = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or_else(bad);
            (x, y)
        })
        .collect()
}

/// Sample pixel colors at each `x,y` in `coords_spec` (PNG pixel space — 1:1
/// with the tree dump's `rect` values today, since the harness renders at
/// `SCALE = 1.0`; see `docs/HEADLESS_UI_HARNESS.md`'s usage note if that ever
/// changes). Prints one `probe (x,y) = #rrggbbaa` line per coordinate to
/// stdout.
pub fn probe_png(png_path: &Path, coords_spec: &str) {
    let img = image::open(png_path)
        .unwrap_or_else(|e| {
            eprintln!("ui-snap probe: open {}: {e}", png_path.display());
            std::process::exit(2);
        })
        .into_rgba8();
    for (x, y) in parse_coords(coords_spec) {
        if x >= img.width() || y >= img.height() {
            eprintln!(
                "ui-snap probe: ({x},{y}) is outside the {}x{} image",
                img.width(),
                img.height()
            );
            std::process::exit(2);
        }
        let px = img.get_pixel(x, y);
        println!("probe ({x},{y}) = #{:02x}{:02x}{:02x}{:02x}", px[0], px[1], px[2], px[3]);
    }
}

/// `cargo xtask ui-snap probe <file.png> --probe x,y[;x,y...]` — standalone
/// form of [`probe_png`].
pub fn cmd_probe(png_path: &str, coords_spec: &str) {
    probe_png(Path::new(png_path), coords_spec);
}

/// Parse `"x,y,w,h"` into a pixel rect. Exits 2 on a malformed spec.
fn parse_rect(spec: &str) -> (i64, i64, i64, i64) {
    let bad = || {
        eprintln!("ui-snap crop: bad rect '{spec}' (expected x,y,w,h)");
        std::process::exit(2);
    };
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() != 4 {
        bad();
    }
    let mut nums = [0i64; 4];
    for (i, p) in parts.iter().enumerate() {
        nums[i] = p.trim().parse().unwrap_or_else(|_| bad());
    }
    (nums[0], nums[1], nums[2], nums[3])
}

/// Crop `png_path` to `rect_spec` (`"x,y,w,h"`, clamped to image bounds) and
/// write `<stem>.crop.png` next to it. Returns the written path.
pub fn crop_png(png_path: &Path, rect_spec: &str) -> PathBuf {
    let img = image::open(png_path).unwrap_or_else(|e| {
        eprintln!("ui-snap crop: open {}: {e}", png_path.display());
        std::process::exit(2);
    });
    let (iw, ih) = (i64::from(img.width()), i64::from(img.height()));
    let (x, y, w, h) = parse_rect(rect_spec);
    let x0 = x.clamp(0, iw);
    let y0 = y.clamp(0, ih);
    let x1 = (x + w).clamp(0, iw);
    let y1 = (y + h).clamp(0, ih);
    let (cw, ch) = ((x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32);
    let cropped = img.crop_imm(x0 as u32, y0 as u32, cw, ch);

    let stem = png_path.file_stem().and_then(|s| s.to_str()).unwrap_or("crop");
    let mut out_path = png_path.to_path_buf();
    out_path.set_file_name(format!("{stem}.crop.png"));
    cropped.save(&out_path).unwrap_or_else(|e| {
        eprintln!("ui-snap crop: save {}: {e}", out_path.display());
        std::process::exit(2);
    });
    println!("ui-snap: wrote {} ({cw}x{ch})", out_path.display());
    out_path
}

/// `cargo xtask ui-snap crop <file.png> --crop x,y,w,h` — standalone form of
/// [`crop_png`].
pub fn cmd_crop(png_path: &str, rect_spec: &str) {
    crop_png(Path::new(png_path), rect_spec);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn dump(nodes: Value) -> Value {
        json!({ "node_count": nodes.as_array().map(Vec::len).unwrap_or(0), "nodes": nodes, "custom_surfaces": [] })
    }

    #[test]
    fn identical_dumps_report_no_changes() {
        let a = dump(json!([{ "id": 1, "rect": [0.0, 0.0, 10.0, 10.0], "bg": "#000000ff" }]));
        let b = a.clone();
        let (lines, differs) = diff_trees(&a, &b);
        assert!(!differs);
        assert_eq!(lines.last().unwrap(), "0 added, 0 removed, 0 changed, 1 unchanged");
    }

    #[test]
    fn changed_bg_is_detected() {
        let a = dump(json!([{ "id": 42, "rect": [10.0, 20.0, 100.0, 30.0], "bg": "#2a2a2dff" }]));
        let b = dump(json!([{ "id": 42, "rect": [10.0, 20.0, 100.0, 30.0], "bg": "#38383cff" }]));
        let (lines, differs) = diff_trees(&a, &b);
        assert!(differs);
        assert!(lines[0].starts_with("node 42 "), "unexpected line: {}", lines[0]);
        assert!(lines[0].contains("bg #2a2a2dff -> #38383cff"), "unexpected line: {}", lines[0]);
        assert_eq!(lines.last().unwrap(), "0 added, 0 removed, 1 changed, 0 unchanged");
    }

    #[test]
    fn added_and_removed_nodes_are_detected() {
        let a = dump(json!([{ "id": 1, "rect": [0.0, 0.0, 10.0, 10.0] }]));
        let b = dump(json!([{ "id": 2, "rect": [0.0, 0.0, 10.0, 10.0] }]));
        let (lines, differs) = diff_trees(&a, &b);
        assert!(differs);
        assert!(lines.iter().any(|l| l == "node 1 [-]: removed"));
        assert!(lines.iter().any(|l| l == "node 2 [-]: added"));
        assert_eq!(lines.last().unwrap(), "1 added, 1 removed, 0 changed, 0 unchanged");
    }

    #[test]
    fn field_present_only_in_one_dump_is_reported() {
        // Union-of-keys guarantee: a field `dump_tree_ex` starts emitting
        // later (here only dump B has it) is diffed, never silently excluded.
        let a = dump(json!([{ "id": 7, "rect": [0.0, 0.0, 10.0, 10.0] }]));
        let b = dump(json!([{ "id": 7, "rect": [0.0, 0.0, 10.0, 10.0], "opacity": 0.5 }]));
        let (lines, differs) = diff_trees(&a, &b);
        assert!(differs);
        assert!(lines[0].contains("opacity null -> 0.5"), "unexpected line: {}", lines[0]);
        assert_eq!(lines.last().unwrap(), "0 added, 0 removed, 1 changed, 0 unchanged");
    }

    #[test]
    fn custom_surfaces_change_is_reported() {
        let nodes = json!([{ "id": 1, "rect": [0.0, 0.0, 10.0, 10.0] }]);
        let a = dump(nodes.clone());
        let mut b = dump(nodes);
        b["custom_surfaces"] = json!([{ "surface_id": "timeline_clips",
            "targets": [{ "kind": "clip", "label": "kick", "rect": [1.0, 2.0, 3.0, 4.0] }] }]);
        let (lines, differs) = diff_trees(&a, &b);
        assert!(differs);
        assert!(
            lines.iter().any(|l| l == "custom_surfaces differ (0 -> 1 targets across 0 -> 1 surfaces)"),
            "lines: {lines:?}"
        );
        assert_eq!(lines.last().unwrap(), "0 added, 0 removed, 0 changed, 1 unchanged");
    }
}
