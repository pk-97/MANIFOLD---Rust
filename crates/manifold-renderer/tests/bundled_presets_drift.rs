//! Bundled effect preset drift detection + regenerator.
//!
//! For every shipping [`ChainSpec`], the directory
//! `crates/manifold-renderer/assets/effect-presets/<EffectTypeId>.json`
//! holds a serialized [`EffectGraphDef`] equal to the result of
//! `spec.build_canonical_graph()` + `EffectGraphDef::from_graph(&g)`.
//!
//! Two tests:
//!
//! - `bundled_presets_match_canonical_splices` runs on every `cargo test`.
//!   For each spec, builds the canonical graph live, serializes it, and
//!   compares against the on-disk JSON. Mismatch fails with a hint to
//!   re-run the regenerator.
//! - `regenerate_bundled_presets` is `#[ignore]`d so it only runs when
//!   the developer explicitly opts in via
//!   `cargo test -p manifold-renderer --test bundled_presets_drift -- --ignored`.
//!   That writes every JSON file from the live splice output.
//!
//! Why split instead of "regenerate when env set":
//! - Default `cargo test` never touches the filesystem in `src/`.
//! - The `--ignored` flag is the standard cargo-test way to gate
//!   destructive / heavy tests without an env-var dance.

use std::fs;
use std::path::PathBuf;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_renderer::node_graph::{ChainSpec, EffectGraphDefExt};

fn presets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("effect-presets")
}

fn preset_path(type_id: &str) -> PathBuf {
    presets_dir().join(format!("{type_id}.json"))
}

fn canonical_def_for(spec: &ChainSpec) -> EffectGraphDef {
    let graph = spec.build_canonical_graph();
    EffectGraphDef::from_graph(&graph)
        .with_name(spec.type_id.as_str())
        .with_description("Canonical default graph generated from ChainSpec::splice.")
}

#[test]
fn bundled_presets_match_canonical_splices() {
    let mut mismatches: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for spec in inventory::iter::<ChainSpec> {
        let path = preset_path(spec.type_id.as_str());
        let live = canonical_def_for(spec);
        let live_json = serde_json::to_string_pretty(&live)
            .expect("EffectGraphDef serializes")
            + "\n";

        if !path.exists() {
            missing.push(format!(
                "  missing: {} (expected at {})",
                spec.type_id.as_str(),
                path.display()
            ));
            continue;
        }

        let on_disk = fs::read_to_string(&path).expect("read bundled preset file");
        if on_disk != live_json {
            mismatches.push(format!(
                "  drift: {} ({})",
                spec.type_id.as_str(),
                path.display()
            ));
        }
    }

    if !missing.is_empty() || !mismatches.is_empty() {
        let mut msg =
            String::from("bundled preset drift vs canonical splice output. Regenerate via:\n");
        msg.push_str(
            "  cargo test -p manifold-renderer --test bundled_presets_drift -- --ignored\n",
        );
        if !missing.is_empty() {
            msg.push_str("\nMissing files:\n");
            msg.push_str(&missing.join("\n"));
        }
        if !mismatches.is_empty() {
            msg.push_str("\nContent drift:\n");
            msg.push_str(&mismatches.join("\n"));
        }
        panic!("{msg}");
    }
}

#[test]
#[ignore = "destructive: writes JSON files into assets/effect-presets/"]
fn regenerate_bundled_presets() {
    let dir = presets_dir();
    fs::create_dir_all(&dir).expect("create assets/effect-presets dir");

    for spec in inventory::iter::<ChainSpec> {
        let def = canonical_def_for(spec);
        let json = serde_json::to_string_pretty(&def).expect("EffectGraphDef serializes") + "\n";
        let path = preset_path(spec.type_id.as_str());
        fs::write(&path, json).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("wrote {}", path.display());
    }
}
