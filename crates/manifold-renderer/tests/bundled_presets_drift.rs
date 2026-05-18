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

use manifold_core::effect_graph_def::{
    AliasEntry, BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata,
    SkipModeDef, ValueAliasEntry,
};
use manifold_core::effect_registration::{
    EffectAliasMetadata, EffectMetadata, EffectValueAliasMetadata,
};
use manifold_renderer::node_graph::{ChainSpec, EffectGraphDefExt, ParamTarget, SkipMode};

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

/// Drift comparison strips `preset_metadata` and normalises `version`
/// — both fields move with the metadata payload (v2 documents have
/// metadata, v1 don't). The splice fn is the source of truth for the
/// graph topology only; metadata is JSON-authoritative (§11). We
/// compare nodes + wires + name + description.
fn graph_topology_only(mut def: EffectGraphDef) -> EffectGraphDef {
    def.preset_metadata = None;
    def.version = 0; // sentinel — version varies between v1 and v2 with metadata
    def
}

#[test]
fn bundled_presets_match_canonical_splices() {
    let mut mismatches: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for spec in inventory::iter::<ChainSpec> {
        let path = preset_path(spec.type_id.as_str());
        let live = canonical_def_for(spec);

        if !path.exists() {
            missing.push(format!(
                "  missing: {} (expected at {})",
                spec.type_id.as_str(),
                path.display()
            ));
            continue;
        }

        let on_disk_raw = fs::read_to_string(&path).expect("read bundled preset file");
        let on_disk: EffectGraphDef = serde_json::from_str(&on_disk_raw)
            .unwrap_or_else(|e| panic!("on-disk preset {} parse: {e}", path.display()));

        // Compare graph topology only — preset_metadata is JSON-
        // authoritative and lives outside the splice fn's responsibility.
        let live_topology = graph_topology_only(live);
        let on_disk_topology = graph_topology_only(on_disk);

        if live_topology != on_disk_topology {
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

/// §11 block 4 one-shot migration helper. Walks every shipping
/// effect's inventory submissions (`EffectMetadata`, `ChainSpec`,
/// `EffectAliasMetadata`, `EffectValueAliasMetadata`) and writes the
/// equivalent `presetMetadata` into the matching JSON file in
/// `assets/effect-presets/`.
///
/// Run once via:
///   cargo test -p manifold-renderer --test bundled_presets_drift \
///     regenerate_preset_metadata_from_inventory -- --ignored
///
/// After this lands, the JSON-derived `EffectDef` overrides the
/// inventory-derived one in `DEFINITIONS`. The
/// `json_metadata_matches_inventory_for_every_migrated_effect` test
/// asserts the two paths produce equivalent results. Block 8 deletes
/// the inventory submissions once `effect_chain_graph.rs` is rewired
/// to use loaded presets instead of `chain_spec_by_id`.
#[test]
#[ignore = "destructive: writes preset_metadata into assets/effect-presets/*.json"]
fn regenerate_preset_metadata_from_inventory() {
    // Build lookup maps for sidecar alias submissions.
    let mut param_aliases_by_id: std::collections::HashMap<
        manifold_core::EffectTypeId,
        Vec<AliasEntry>,
    > = std::collections::HashMap::new();
    for sidecar in inventory::iter::<EffectAliasMetadata> {
        param_aliases_by_id.insert(
            sidecar.id.clone(),
            sidecar
                .aliases
                .iter()
                .map(|(old, new)| AliasEntry {
                    old: (*old).to_string(),
                    new: new.map(|s| s.to_string()),
                })
                .collect(),
        );
    }
    let mut value_aliases_by_id: std::collections::HashMap<
        manifold_core::EffectTypeId,
        Vec<ValueAliasEntry>,
    > = std::collections::HashMap::new();
    for sidecar in inventory::iter::<EffectValueAliasMetadata> {
        let entries: Vec<ValueAliasEntry> = sidecar
            .aliases
            .iter()
            .map(|(param_id, mapping)| ValueAliasEntry {
                param_id: (*param_id).to_string(),
                mapping: mapping.iter().copied().collect(),
            })
            .collect();
        value_aliases_by_id.insert(sidecar.id.clone(), entries);
    }

    // Index ChainSpecs by id so we can pair each EffectMetadata with
    // its matching bindings + skip_mode.
    let mut specs_by_id: std::collections::HashMap<manifold_core::EffectTypeId, &'static ChainSpec> =
        std::collections::HashMap::new();
    for spec in inventory::iter::<ChainSpec> {
        specs_by_id.insert(spec.type_id.clone(), spec);
    }

    let mut written = 0usize;
    for meta in inventory::iter::<EffectMetadata> {
        let path = preset_path(meta.id.as_str());
        if !path.exists() {
            eprintln!(
                "skip {} — no JSON file at {}",
                meta.id.as_str(),
                path.display()
            );
            continue;
        }

        let spec = specs_by_id.get(&meta.id).unwrap_or_else(|| {
            panic!(
                "EffectMetadata for {} has no matching ChainSpec — \
                 can't migrate bindings/skip_mode",
                meta.id.as_str()
            )
        });

        let params: Vec<ParamSpecDef> = meta
            .params
            .iter()
            .map(|p| ParamSpecDef {
                id: p.id.to_string(),
                name: p.name.to_string(),
                min: p.min,
                max: p.max,
                default_value: p.default_value,
                whole_numbers: p.whole_numbers,
                is_toggle: p.is_toggle,
                value_labels: p.value_labels.iter().map(|s| s.to_string()).collect(),
                format_string: p.format_string.map(|s| s.to_string()),
                osc_suffix: p.osc_suffix.to_string(),
            })
            .collect();

        let bindings: Vec<BindingDef> = spec
            .bindings
            .iter()
            .map(|b| BindingDef {
                id: b.id.to_string(),
                label: b.label.to_string(),
                default_value: b.default_value,
                target: match &b.target {
                    ParamTarget::HandleNode { handle, param } => BindingTarget::HandleNode {
                        handle: (*handle).to_string(),
                        param: (*param).to_string(),
                    },
                    ParamTarget::Composite { outer_name } => BindingTarget::Composite {
                        outer_name: outer_name.to_string(),
                    },
                    other => panic!(
                        "{}: binding {:?} uses {:?} variant which has no JSON form",
                        meta.id.as_str(),
                        b.id,
                        other
                    ),
                },
                convert: b.convert,
            })
            .collect();

        let skip_mode = match spec.skip {
            SkipMode::Never => SkipModeDef::Never,
            SkipMode::OnZero { param_id } => SkipModeDef::OnZero {
                param_id: param_id.to_string(),
            },
        };

        let preset_metadata = PresetMetadata {
            id: meta.id.clone(),
            display_name: meta.display_name.to_string(),
            category: meta.category.to_string(),
            osc_prefix: meta.osc_prefix.to_string(),
            legacy_discriminant: meta.legacy_discriminant,
            available: meta.available,
            params,
            bindings,
            skip_mode,
            param_aliases: param_aliases_by_id
                .get(&meta.id)
                .cloned()
                .unwrap_or_default(),
            node_aliases: Vec::new(),
            value_aliases: value_aliases_by_id
                .get(&meta.id)
                .cloned()
                .unwrap_or_default(),
        };

        // Read the existing JSON to preserve topology, then attach
        // (or replace) preset_metadata.
        let raw = fs::read_to_string(&path).expect("read existing preset");
        let existing: EffectGraphDef =
            serde_json::from_str(&raw).expect("parse existing preset");
        let merged = existing.with_preset_metadata(preset_metadata);

        let json = serde_json::to_string_pretty(&merged).expect("serialize") + "\n";
        fs::write(&path, json).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("migrated {}", meta.id.as_str());
        written += 1;
    }

    println!("regenerate_preset_metadata_from_inventory: wrote {written} files");
}

#[test]
#[ignore = "destructive: writes JSON files into assets/effect-presets/"]
fn regenerate_bundled_presets() {
    let dir = presets_dir();
    fs::create_dir_all(&dir).expect("create assets/effect-presets dir");

    for spec in inventory::iter::<ChainSpec> {
        let mut def = canonical_def_for(spec);

        // Preserve any existing preset_metadata — that field is JSON-
        // authoritative and the splice fn doesn't produce it.
        // Regeneration should refresh the graph topology without
        // clobbering metadata that block 4 has migrated in.
        let path = preset_path(spec.type_id.as_str());
        if path.exists() {
            let existing_raw = fs::read_to_string(&path).expect("read existing preset");
            if let Ok(existing) = serde_json::from_str::<EffectGraphDef>(&existing_raw) {
                if let Some(metadata) = existing.preset_metadata {
                    def = def.with_preset_metadata(metadata);
                }
            }
        }

        let json = serde_json::to_string_pretty(&def).expect("EffectGraphDef serializes") + "\n";
        fs::write(&path, json).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        println!("wrote {}", path.display());
    }
}
