//! Workspace dependency boundaries from RENDERER_CRATE_SPLIT_DESIGN.md section 3.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

struct Layer {
    package: &'static str,
    normal_and_build: &'static [&'static str],
    dev: &'static [&'static str],
}

const UI_PAINT_DEPS: &[&str] = &[
    "manifold-gpu", "manifold-ui", "manifold-foundation", "manifold-core",
];

// Each later split phase adds its owning crate and allowed workspace edges.
const LAYERS: &[Layer] = &[
    Layer {
        package: "manifold-compositor",
        normal_and_build: &["manifold-core", "manifold-gpu", "manifold-node-engine", "manifold-playback"],
        dev: &["manifold-node-engine"],
    },
    Layer {
        package: "manifold-nodes",
        normal_and_build: &["manifold-core", "manifold-gpu", "manifold-node-engine", "manifold-nodes-image", "manifold-nodes-scene", "manifold-nodes-water", "manifold-water-gpu-mpm", "manifold-water-liquid", "manifold-water-whitewater"],
        dev: &["manifold-fluids", "manifold-foundation", "manifold-nodes", "manifold-gpu", "manifold-node-engine", "manifold-nodes-image", "manifold-nodes-scene", "manifold-nodes-water", "manifold-physics", "manifold-playback", "manifold-water-gpu-flip", "manifold-water-gpu-mpm", "manifold-water-liquid", "manifold-water-rigid", "manifold-water-surface", "manifold-water-whitewater"],
    },
    Layer {
        package: "manifold-app",
        normal_and_build: &["manifold-audio", "manifold-compositor", "manifold-core", "manifold-editing", "manifold-gpu", "manifold-io", "manifold-led", "manifold-media", "manifold-node-engine", "manifold-nodes-image", "manifold-nodes-scene", "manifold-nodes-water", "manifold-playback", "manifold-profiler", "manifold-recording", "manifold-nodes", "manifold-spectral", "manifold-ui", "manifold-ui-paint"],
        dev: &["manifold-foundation", "manifold-physics", "manifold-fluids", "manifold-nodes", "manifold-compositor", "manifold-node-engine", "manifold-nodes-image", "manifold-nodes-scene", "manifold-nodes-water", "manifold-water-liquid", "manifold-water-rigid"],
    },

    Layer {
        package: "manifold-nodes-scene",
        normal_and_build: &["manifold-core", "manifold-foundation", "manifold-gpu", "manifold-node-engine"],
        dev: &["manifold-node-engine", "manifold-nodes-scene"],
    },

    Layer {
        package: "manifold-nodes-image",
        normal_and_build: &["manifold-core", "manifold-foundation", "manifold-gpu", "manifold-native", "manifold-node-engine"],
        dev: &["manifold-node-engine", "manifold-nodes-image"],
    },

    Layer {
        package: "manifold-node-engine",
        normal_and_build: &["manifold-foundation", "manifold-core", "manifold-gpu",
                            "manifold-native", "manifold-playback"],
        dev: &["manifold-nodes"],
    },
    Layer {
        package: "manifold-nodes-water",
        normal_and_build: &["manifold-core", "manifold-foundation", "manifold-gpu",
                            "manifold-node-engine", "manifold-physics", "manifold-fluids",
                            "manifold-water-gpu-flip", "manifold-water-gpu-mpm", "manifold-water-liquid", "manifold-water-rigid", "manifold-water-surface", "manifold-water-whitewater"],
        dev: &["manifold-node-engine", "manifold-playback", "manifold-water-gpu-flip", "manifold-water-gpu-mpm", "manifold-water-liquid", "manifold-water-rigid", "manifold-water-surface", "manifold-water-whitewater"],
    },
    // D1: a leaf solver sits on the liquid seam and names no other solver.
    Layer {
        package: "manifold-water-gpu-flip",
        normal_and_build: &["manifold-core", "manifold-gpu", "manifold-node-engine", "manifold-physics",
                            "manifold-water-liquid", "manifold-water-rigid"],
        dev: &["manifold-fluids", "manifold-node-engine", "manifold-water-gpu-flip", "manifold-water-liquid", "manifold-water-rigid"],
    },
    Layer {
        package: "manifold-water-gpu-mpm",
        normal_and_build: &["manifold-core", "manifold-gpu", "manifold-node-engine", "manifold-physics",
                            "manifold-water-liquid", "manifold-water-rigid"],
        dev: &["manifold-node-engine", "manifold-water-gpu-mpm", "manifold-water-liquid"],
    },
    Layer {
        package: "manifold-water-whitewater",
        normal_and_build: &["manifold-core", "manifold-fluids", "manifold-gpu", "manifold-node-engine",
                            "manifold-physics", "manifold-water-liquid"],
        dev: &["manifold-node-engine", "manifold-water-liquid", "manifold-water-whitewater"],
    },
    Layer {
        package: "manifold-water-surface",
        normal_and_build: &["manifold-core", "manifold-gpu", "manifold-node-engine", "manifold-water-liquid"],
        dev: &["manifold-node-engine", "manifold-water-liquid", "manifold-water-surface"],
    },
    // D1: the liquid seam sits on rigid and under every solver. manifold-fluids is a
    // normal edge: fluid role geometry validates closed meshes through it (D1 amended).
    Layer {
        package: "manifold-water-liquid",
        normal_and_build: &["manifold-core", "manifold-fluids", "manifold-foundation", "manifold-gpu",
                            "manifold-node-engine", "manifold-physics", "manifold-water-rigid"],
        dev: &["manifold-node-engine", "manifold-water-liquid", "manifold-water-rigid"],
    },
    // WATER_CRATES_DESIGN.md D1: rigid sits below every liquid and names none of them.
    Layer {
        package: "manifold-water-rigid",
        normal_and_build: &["manifold-core", "manifold-foundation", "manifold-gpu",
                            "manifold-node-engine", "manifold-physics"],
        dev: &["manifold-fluids", "manifold-node-engine", "manifold-water-rigid"],
    },
    Layer {
        package: "manifold-ui-paint",
        normal_and_build: UI_PAINT_DEPS,
        dev: UI_PAINT_DEPS,
    },
];

#[test]
fn workspace_dependencies_obey_layering() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap();
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .output()
        .expect("cargo metadata must run");
    assert!(output.status.success(), "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr));
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("cargo metadata must return JSON");
    let members: BTreeSet<_> = metadata["workspace_members"].as_array().unwrap()
        .iter().map(|id| id.as_str().unwrap()).collect();
    let packages: BTreeMap<_, _> = metadata["packages"].as_array().unwrap().iter()
        .filter(|package| members.contains(package["id"].as_str().unwrap()))
        .map(|package| (package["name"].as_str().unwrap(), package)).collect();
    let mut normal_and_build = BTreeSet::new();
    let mut dev = BTreeSet::new();
    for (name, package) in &packages {
        for dependency in package["dependencies"].as_array().unwrap() {
            let target = dependency["name"].as_str().unwrap();
            if !packages.contains_key(target) {
                continue;
            }
            match dependency["kind"].as_str() {
                None | Some("normal") | Some("build") => {
                    normal_and_build.insert((*name, target));
                }
                Some("dev") => { dev.insert((*name, target)); }
                Some(kind) => panic!("unknown dependency kind: {kind}"),
            }
        }
    }
    for source in ["manifold-nodes", "manifold-app"] {
        assert!(normal_and_build.contains(&(source, "manifold-nodes-image")),
                "missing leaf dependency: {source} -> manifold-nodes-image");
    }
    for source in ["manifold-nodes", "manifold-app"] {
        assert!(normal_and_build.contains(&(source, "manifold-nodes-scene")),
                "missing leaf dependency: {source} -> manifold-nodes-scene");
    }
    for source in ["manifold-nodes", "manifold-app"] {
        assert!(normal_and_build.contains(&(source, "manifold-nodes-water")),
                "missing water dependency: {source} -> manifold-nodes-water");
    }
    assert!(normal_and_build.contains(&("manifold-app", "manifold-compositor")),
            "missing leaf dependency: manifold-app -> manifold-compositor");
    for layer in LAYERS {
        assert!(packages.contains_key(layer.package), "missing crate: {}", layer.package);
        for (kind, edges, allowed) in [
            ("normal/build", &normal_and_build, layer.normal_and_build),
            ("dev", &dev, layer.dev),
        ] {
            for &(source, target) in edges {
                if source == layer.package {
                    assert!(allowed.contains(&target),
                            "forbidden {kind} dependency: {source} -> {target}");
                }
            }
        }
    }
}

/// Water vocabulary leaving the engine is a ratchet: this count may only go
/// down. Lower it when a move lands; never raise it. Obsolete when the engine
/// names no water words at all.
const ENGINE_WATER_WORD_FILES: usize = 54;
const ENGINE_WATER_WORD_EXEMPT: &[&str] = &["atomic/fluid_sim_2d", "param_tooltips", "trigger_shadow_lint"];

fn engine_water_word_files(dir: &Path, root: &Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            engine_water_word_files(&path, root, hits);
            continue;
        }
        let relative = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if ENGINE_WATER_WORD_EXEMPT.iter().any(|exempt| relative.contains(exempt)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let text = text.to_lowercase();
        if ["fluid", "liquid", "physics", "whitewater"].iter().any(|word| text.contains(word)) {
            hits.push(relative);
        }
    }
}

#[test]
fn engine_water_vocabulary_only_shrinks() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../manifold-node-engine/src");
    let mut hits = Vec::new();
    engine_water_word_files(&src, &src, &mut hits);
    hits.sort();
    assert!(hits.len() <= ENGINE_WATER_WORD_FILES,
        "engine files naming water words rose to {} (ratchet {ENGINE_WATER_WORD_FILES}); move the water behaviour to manifold-nodes-water instead:\n{}",
        hits.len(), hits.join("\n"));
    assert!(hits.len() >= ENGINE_WATER_WORD_FILES,
        "engine water-word files fell to {}; lower ENGINE_WATER_WORD_FILES to match", hits.len());
}

/// INV-W2 (WATER_CRATES_DESIGN.md section 8): the rigid crate's code names no
/// liquid solver. Comments are skipped; the allowlist is the pair contract
/// (D2) and the coupled-step tests that use manifold-fluids as their oracle.
const RIGID_LIQUID_WORD_EXEMPT: &[&str] = &["node.rs", "physics/coupling_tests.rs"];

fn names_a_liquid(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("").to_lowercase();
    ["liquid", "whitewater", "gpu_flip"].iter().any(|word| code.contains(word))
        || code.match_indices("matter").any(|(at, _)| {
            !code[..at].chars().next_back().is_some_and(|c| c.is_ascii_alphabetic())
        })
}

fn rigid_liquid_word_files(dir: &Path, root: &Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rigid_liquid_word_files(&path, root, hits);
            continue;
        }
        let relative = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if RIGID_LIQUID_WORD_EXEMPT.contains(&relative.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (index, line) in text.lines().enumerate() {
            if names_a_liquid(line) {
                hits.push(format!("{relative}:{}: {}", index + 1, line.trim()));
            }
        }
    }
}

#[test]
fn rigid_names_no_liquid() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../manifold-water-rigid/src");
    let mut hits = Vec::new();
    rigid_liquid_word_files(&src, &src, &mut hits);
    hits.sort();
    assert!(hits.is_empty(), "manifold-water-rigid names a liquid solver; the liquids depend on rigid, never the reverse:\n{}",
        hits.join("\n"));
}

/// Extra feature flags that fold into testkit/gpu-proofs with BUG-hkbdp.6.9
/// (proofs consolidation). The pin drops to exactly two when 6.9 closes.
const WATER_EXTRA_FEATURES: &[(&str, &str)] = &[
    ("manifold-nodes-water", "fluid-perf-proofs"),
    ("manifold-nodes-water", "whitewater-oracle"),
];

/// INV-W3: every water crate declares exactly `testkit` and `gpu-proofs`.
#[test]
fn water_crates_declare_exactly_testkit_and_gpu_proofs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .output()
        .expect("cargo metadata must run");
    assert!(output.status.success(), "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr));
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("cargo metadata must return JSON");
    let mut water = 0;
    for package in metadata["packages"].as_array().unwrap() {
        let name = package["name"].as_str().unwrap();
        if !(name.starts_with("manifold-water-") || name == "manifold-nodes-water") {
            continue;
        }
        water += 1;
        let features: BTreeSet<&str> = package["features"].as_object().unwrap()
            .keys().map(String::as_str).collect();
        let expected: BTreeSet<&str> = ["testkit", "gpu-proofs"].into_iter()
            .chain(WATER_EXTRA_FEATURES.iter().filter(|(crate_name, _)| *crate_name == name)
                .map(|(_, feature)| *feature))
            .collect();
        assert_eq!(features, expected, "{name} features drifted from testkit + gpu-proofs");
    }
    assert!(water >= 2, "water crates missing from cargo metadata");
}

/// INV-W2 liquid companion (WATER_CRATES_DESIGN.md section 4.2): the seam's
/// code names no solver. Comments and node descriptor prose are skipped;
/// `whitewater.rs` owns the grid vocabulary by name, and `clock.rs` keeps the
/// table of each solver's interval-duration input by type id.
const LIQUID_SOLVER_WORD_EXEMPT: &[&str] = &["whitewater.rs", "clock.rs"];

fn names_a_solver(line: &str) -> bool {
    let trimmed = line.trim_start();
    if ["purpose:", "composition_notes:", "summary:"].iter().any(|field| trimmed.starts_with(field)) {
        return false;
    }
    let code = line.split("//").next().unwrap_or("");
    ["gpu_flip", "matter_", "whitewater_step"].iter().any(|word| code.contains(word))
}

fn liquid_solver_word_files(dir: &Path, root: &Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            liquid_solver_word_files(&path, root, hits);
            continue;
        }
        let relative = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        if LIQUID_SOLVER_WORD_EXEMPT.contains(&relative.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (index, line) in text.lines().enumerate() {
            if names_a_solver(line) {
                hits.push(format!("{relative}:{}: {}", index + 1, line.trim()));
            }
        }
    }
}

#[test]
fn liquid_names_no_solver() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../manifold-water-liquid/src");
    let mut hits = Vec::new();
    liquid_solver_word_files(&src, &src, &mut hits);
    hits.sort();
    assert!(hits.is_empty(), "manifold-water-liquid names a solver; the solvers depend on the seam, never the reverse:\n{}",
        hits.join("\n"));
}
