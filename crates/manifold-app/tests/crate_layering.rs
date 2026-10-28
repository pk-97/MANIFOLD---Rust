//! Workspace dependency boundaries from the crate layering rules.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

struct Layer {
    package: &'static str,
    normal_and_build: &'static [&'static str],
    dev: &'static [&'static str],
}

const UI_PAINT_DEPS: &[&str] = &[
    "manifold-gpu",
    "manifold-ui",
    "manifold-foundation",
    "manifold-core",
];

// Each later split phase adds its owning crate and allowed workspace edges.
const LAYERS: &[Layer] = &[
    Layer {
        package: "manifold-compositor",
        normal_and_build: &[
            "manifold-core",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-playback",
        ],
        dev: &["manifold-node-engine"],
    },
    Layer {
        package: "manifold-nodes",
        normal_and_build: &[
            "manifold-core",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-image",
            "manifold-nodes-physics",
            "manifold-nodes-scene",
            "manifold-nodes-water",
            "manifold-water-gpu-mpm",
            "manifold-water-liquid",
            "manifold-water-whitewater",
        ],
        // manifold-editing: the scene document crash test drives editing commands through the compiler.
        dev: &[
            "manifold-editing",
            "manifold-fluids",
            "manifold-foundation",
            "manifold-nodes",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-image",
            "manifold-nodes-scene",
            "manifold-nodes-water",
            "manifold-physics",
            "manifold-playback",
            "manifold-water-gpu-flip",
            "manifold-water-gpu-mpm",
            "manifold-water-liquid",
            "manifold-water-rigid",
            "manifold-water-surface",
            "manifold-water-whitewater",
        ],
    },
    Layer {
        package: "manifold-app",
        normal_and_build: &[
            "manifold-audio",
            "manifold-compositor",
            "manifold-core",
            "manifold-editing",
            "manifold-foundation",
            "manifold-gpu",
            "manifold-io",
            "manifold-led",
            "manifold-media",
            "manifold-node-engine",
            "manifold-nodes-image",
            "manifold-nodes-scene",
            "manifold-nodes-water",
            "manifold-playback",
            "manifold-profiler",
            "manifold-recording",
            "manifold-nodes",
            "manifold-spectral",
            "manifold-ui",
            "manifold-ui-paint",
        ],
        dev: &[
            "manifold-editing",
            "manifold-foundation",
            "manifold-physics",
            "manifold-fluids",
            "manifold-nodes",
            "manifold-compositor",
            "manifold-node-engine",
            "manifold-nodes-image",
            "manifold-nodes-scene",
            "manifold-nodes-water",
            "manifold-playback",
            "manifold-water-liquid",
            "manifold-water-rigid",
        ],
    },
    Layer {
        package: "manifold-nodes-scene",
        normal_and_build: &[
            "manifold-core",
            "manifold-foundation",
            "manifold-gpu",
            "manifold-node-engine",
        ],
        dev: &["manifold-node-engine", "manifold-nodes-scene"],
    },
    Layer {
        package: "manifold-nodes-image",
        normal_and_build: &[
            "manifold-core",
            "manifold-foundation",
            "manifold-gpu",
            "manifold-native",
            "manifold-node-engine",
        ],
        dev: &["manifold-node-engine", "manifold-nodes-image"],
    },
    Layer {
        package: "manifold-node-engine",
        normal_and_build: &[
            "manifold-foundation",
            "manifold-core",
            "manifold-gpu",
            "manifold-native",
            "manifold-playback",
        ],
        dev: &["manifold-nodes"],
    },
    Layer {
        package: "manifold-nodes-water",
        normal_and_build: &[
            "manifold-core",
            "manifold-foundation",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-physics",
            "manifold-physics",
            "manifold-fluids",
            "manifold-water-gpu-flip",
            "manifold-water-gpu-mpm",
            "manifold-water-liquid",
            "manifold-water-rigid",
            "manifold-water-surface",
            "manifold-water-whitewater",
        ],
        dev: &[
            "manifold-node-engine",
            "manifold-playback",
            "manifold-water-gpu-flip",
            "manifold-water-gpu-mpm",
            "manifold-water-liquid",
            "manifold-water-rigid",
            "manifold-water-surface",
            "manifold-water-whitewater",
        ],
    },
    // D1: a leaf solver sits on the liquid seam and names no other solver.
    Layer {
        package: "manifold-water-gpu-flip",
        normal_and_build: &[
            "manifold-core",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-physics",
            "manifold-physics",
            "manifold-water-liquid",
            "manifold-water-rigid",
        ],
        dev: &[
            "manifold-fluids",
            "manifold-node-engine",
            "manifold-water-gpu-flip",
            "manifold-water-liquid",
            "manifold-water-rigid",
        ],
    },
    Layer {
        package: "manifold-water-gpu-mpm",
        normal_and_build: &[
            "manifold-core",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-physics",
            "manifold-physics",
            "manifold-water-liquid",
            "manifold-water-rigid",
        ],
        dev: &[
            "manifold-node-engine",
            "manifold-water-gpu-mpm",
            "manifold-water-liquid",
        ],
    },
    Layer {
        package: "manifold-water-whitewater",
        normal_and_build: &[
            "manifold-core",
            "manifold-fluids",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-physics",
            "manifold-physics",
            "manifold-water-liquid",
        ],
        dev: &[
            "manifold-node-engine",
            "manifold-water-liquid",
            "manifold-water-whitewater",
        ],
    },
    Layer {
        package: "manifold-water-surface",
        normal_and_build: &[
            "manifold-core",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-water-liquid",
        ],
        dev: &[
            "manifold-node-engine",
            "manifold-water-liquid",
            "manifold-water-surface",
        ],
    },
    // D1: the liquid seam sits on rigid and under every solver. manifold-fluids is a
    // normal edge: fluid role geometry validates closed meshes through it (D1 amended).
    Layer {
        package: "manifold-water-liquid",
        normal_and_build: &[
            "manifold-core",
            "manifold-fluids",
            "manifold-foundation",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-nodes-physics",
            "manifold-physics",
            "manifold-water-rigid",
        ],
        dev: &[
            "manifold-node-engine",
            "manifold-water-liquid",
            "manifold-water-rigid",
        ],
    },
    // D1: the shared physics nodes (force lattice, swarms) sit under every
    // liquid and name none of them.
    Layer {
        package: "manifold-nodes-physics",
        normal_and_build: &["manifold-gpu", "manifold-node-engine", "manifold-physics"],
        dev: &["manifold-foundation", "manifold-node-engine"],
    },
    // Rigid sits below every liquid and names none of them.
    Layer {
        package: "manifold-water-rigid",
        normal_and_build: &[
            "manifold-core",
            "manifold-foundation",
            "manifold-gpu",
            "manifold-node-engine",
            "manifold-physics",
        ],
        dev: &[
            "manifold-fluids",
            "manifold-node-engine",
            "manifold-water-rigid",
        ],
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
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .output()
        .expect("cargo metadata must run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must return JSON");
    let members: BTreeSet<_> = metadata["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    let packages: BTreeMap<_, _> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|package| members.contains(package["id"].as_str().unwrap()))
        .map(|package| (package["name"].as_str().unwrap(), package))
        .collect();
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
                Some("dev") => {
                    dev.insert((*name, target));
                }
                Some(kind) => panic!("unknown dependency kind: {kind}"),
            }
        }
    }
    for source in ["manifold-nodes", "manifold-app"] {
        assert!(
            normal_and_build.contains(&(source, "manifold-nodes-image")),
            "missing leaf dependency: {source} -> manifold-nodes-image"
        );
    }
    for source in ["manifold-nodes", "manifold-app"] {
        assert!(
            normal_and_build.contains(&(source, "manifold-nodes-scene")),
            "missing leaf dependency: {source} -> manifold-nodes-scene"
        );
    }
    for source in ["manifold-nodes", "manifold-app"] {
        assert!(
            normal_and_build.contains(&(source, "manifold-nodes-water")),
            "missing water dependency: {source} -> manifold-nodes-water"
        );
    }
    assert!(
        normal_and_build.contains(&("manifold-app", "manifold-compositor")),
        "missing leaf dependency: manifold-app -> manifold-compositor"
    );
    for layer in LAYERS {
        assert!(
            packages.contains_key(layer.package),
            "missing crate: {}",
            layer.package
        );
        for (kind, edges, allowed) in [
            ("normal/build", &normal_and_build, layer.normal_and_build),
            ("dev", &dev, layer.dev),
        ] {
            for &(source, target) in edges {
                if source == layer.package {
                    assert!(
                        allowed.contains(&target),
                        "forbidden {kind} dependency: {source} -> {target}"
                    );
                }
            }
        }
    }
}
