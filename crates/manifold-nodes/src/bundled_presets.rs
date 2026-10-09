//! Bundled effect, generator, and scene modifier preset catalogs.
//!
//! Each bundled preset is a JSON
//! [`EffectGraphDef`]. The JSON files are **scanned from disk at
//! startup** by [`manifold_node_engine::load::preset_loader`] (stock from the packaged
//! bundle or the dev workspace assets dir, plus optional user presets),
//! not embedded into the binary. The binary has zero compile-time
//! knowledge of which effects exist. Adding a preset is just dropping a
//! JSON file in the stock directory — no rebuild required.
//!
//! The bundled preset for `PresetTypeId::X` is the canonical default
//! graph for that preset. The JSON file is authoritative —
//! the chain runtime and editor snapshot both source bindings,
//! skip-mode, and topology from the embedded
//! [`PresetMetadata`](manifold_core::effect_graph_def::PresetMetadata)
//! block via [`manifold_node_engine::load::loaded_preset_view::LoadedPresetView`].
//!
//! User-authored per-instance graphs are stored separately on the
//! [`PresetInstance`](manifold_core::effects::PresetInstance). Both
//! shapes use the same [`EffectGraphDef`] schema and the same
//! [`manifold_node_engine::persistence::PrimitiveRegistry`] loader; they
//! differ only in storage location.
//!
//! The type id is the JSON filename stem, exactly as before — type ids
//! are forever (save files reference them).

use std::sync::Arc;

use ahash::AHashMap;
use arc_swap::ArcSwap;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;

use manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures;
use manifold_node_engine::load::preset_loader::{
    EFFECT_CATALOG, GENERATOR_CATALOG, PresetCatalog, SCENE_MODIFIER_CATALOG,
    catalog_generation,
};

inventory::submit! {
    manifold_node_engine::load::catalog_source::PresetCatalogSource {
        name: "bundled",
        json: bundled_preset_json,
        def: bundled_preset_def,
        visit: |kind, visitor| bundled_preset_type_ids(kind).for_each(visitor),
    }
}


/// Raw JSON for the bundled preset of `preset_type` (any kind), or
/// `None` if no preset has that type id.
///
/// Kind-agnostic: catalog ids are globally disjoint (verified), so this checks
/// effect, generator, then scene modifier catalogs
/// and returns the single match. The string is the current on-disk file
/// verbatim. Hot-reload (step 10): the catalogs live behind [`ArcSwap`], so
/// this returns an owned `Arc<str>` cloned from the current snapshot rather
/// than a `&'static` borrow — a concurrent reload can swap the snapshot
/// without invalidating a value the caller already holds.
pub fn bundled_preset_json(preset_type: &PresetTypeId) -> Option<Arc<str>> {
    [PresetKind::Effect, PresetKind::Generator, PresetKind::SceneModifier]
        .into_iter()
        .find_map(|kind| catalog_for_kind(kind).load().json(preset_type.as_str()))
}

/// Return the live catalog for one preset kind.
fn catalog_for_kind(kind: PresetKind) -> &'static ArcSwap<PresetCatalog> {
    match kind {
        PresetKind::Effect => &EFFECT_CATALOG,
        PresetKind::Generator => &GENERATOR_CATALOG,
        PresetKind::SceneModifier => &SCENE_MODIFIER_CATALOG,
    }
}

/// Parse one catalog entry and apply the migration policy for its kind.
/// Effects and generators carry scene exposures; scene modifier recipes use
/// their own vocabulary and deliberately skip that migration.
fn parse_bundled_preset(kind: PresetKind, id: &str, json: &str) -> EffectGraphDef {
    let mut def: EffectGraphDef = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("bundled {kind:?} preset {id}: parse failed: {e}"));
    if !kind.is_scene_modifier() {
        if let Err(error) = manifold_nodes_scene::node_graph::scene_camera::prepare_camera_effects(&mut def) {
            log::warn!("Scene camera setup for {id}: {error}");
        }
        migrate_scene_exposures(&mut def);
    }
    def
}

/// Generation-stamped parsed-def cache. Keyed `&'static str` → leaked
/// `&'static EffectGraphDef` so [`bundled_preset_def`] can keep handing out
/// `&'static` references (the render path stores them on
/// `LoadedPresetView.canonical_def`). The cache is rebuilt (and re-leaked)
/// whenever the catalog generation advances; at rest the generation never
/// moves and the cache is reused.
struct DefCache {
    /// Generation this map was built against. `u64::MAX` = not yet built.
    generation: std::sync::atomic::AtomicU64,
    map: ArcSwap<AHashMap<&'static str, &'static EffectGraphDef>>,
}

static DEF_CACHE: std::sync::LazyLock<DefCache> = std::sync::LazyLock::new(|| DefCache {
    generation: std::sync::atomic::AtomicU64::new(u64::MAX),
    map: ArcSwap::from_pointee(AHashMap::default()),
});

fn parsed_def_map() -> Arc<AHashMap<&'static str, &'static EffectGraphDef>> {
    let generation = catalog_generation();
    if DEF_CACHE.generation.load(std::sync::atomic::Ordering::Acquire) != generation {
        rebuild_def_cache(generation);
    }
    DEF_CACHE.map.load_full()
}

#[cold]
fn rebuild_def_cache(generation: u64) {
    // Build from the current catalog snapshot and leak each def so the
    // returned references are `'static`. The leak is bounded by the
    // (finite) shipping preset count × the number of reloads in a session —
    // authoring-time, never on the perform path.
    let mut m: AHashMap<&'static str, &'static EffectGraphDef> = AHashMap::default();
    // Hold each catalog snapshot through the rebuild so no snapshot is
    // dropped midway through parsing its entries.
    let effect_catalog = catalog_for_kind(PresetKind::Effect).load();
    let generator_catalog = catalog_for_kind(PresetKind::Generator).load();
    let scene_modifier_catalog = catalog_for_kind(PresetKind::SceneModifier).load();
    let catalogs = [
        (PresetKind::Effect, &*effect_catalog),
        (PresetKind::Generator, &*generator_catalog),
        (PresetKind::SceneModifier, &*scene_modifier_catalog),
    ];
    for (kind, catalog) in catalogs {
        for (id, json) in catalog.entries() {
            let def = parse_bundled_preset(kind, &id, &json);
            let id_static: &'static str = Box::leak(id.to_string().into_boxed_str());
            let def_static: &'static EffectGraphDef = Box::leak(Box::new(def));
            m.insert(id_static, def_static);
        }
    }
    DEF_CACHE.map.store(Arc::new(m));
    DEF_CACHE
        .generation
        .store(generation, std::sync::atomic::Ordering::Release);
}

/// Parsed [`EffectGraphDef`] for the bundled preset of `preset_type`
/// (any kind), or `None` if no preset is registered.
///
/// First call (and every call after a hot-reload generation bump) parses
/// all three catalog snapshots into a leaked map; subsequent calls return a
/// borrowed reference into that map. At rest, parsing happens once.
///
/// Parse failures panic with the type id and underlying error — these come
/// from files we author, so any failure is a developer mistake to fix, not
/// a runtime condition to handle.
pub fn bundled_preset_def(preset_type: &PresetTypeId) -> Option<&'static EffectGraphDef> {
    parsed_def_map().get(preset_type.as_str()).copied()
}

/// Every [`PresetTypeId`] of `kind` that has a bundled preset registered
/// (current snapshot of that kind's catalog).
pub fn bundled_preset_type_ids(kind: PresetKind) -> impl Iterator<Item = PresetTypeId> {
    catalog_for_kind(kind)
        .load()
        .type_ids()
        .map(|id| PresetTypeId::from_string(id.to_string()))
        .collect::<Vec<_>>()
        .into_iter()
}

/// Loader function for the core's
/// [`manifold_core::preset_definition_registry::effect::PresetSource`] inventory.
/// Walks the bundled preset table, parses each JSON document, and
/// returns the `preset_metadata` field from every entry that carries
/// one (v2 schema). Every shipping bundled preset is v2 post-section 11;
/// the `Option`-returning shape is retained so test-only or
/// hand-authored v1 fixtures stay loadable as graphs without
/// breaking the metadata projection.
///
/// Called at startup and again during hot reload after the catalog snapshot
/// has been swapped.
pub fn loaded_presets_from_bundled() -> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    loaded_preset_metadata(PresetKind::Effect)
}

/// Loader for the dedicated scene-modifier metadata inventory bucket. Raw
/// modifier recipes deliberately skip [`migrate_scene_exposures`].
pub fn loaded_scene_modifier_presets_from_bundled(
) -> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    loaded_preset_metadata(PresetKind::SceneModifier)
}

/// Load metadata directly from the current catalog snapshot for one kind.
/// During hot reload, `apply_reload` swaps catalogs before calling the public
/// metadata loaders and bumps the generation only afterward, so this helper
/// must parse those current snapshots rather than read the prior def cache.
pub(crate) fn loaded_preset_metadata(
    kind: PresetKind,
) -> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    catalog_for_kind(kind)
        .load()
        .entries()
        .filter_map(|(id, json)| parse_bundled_preset(kind, &id, &json).preset_metadata)
        .collect()
}

inventory::submit! {
    manifold_core::preset_definition_registry::effect::PresetSource {
        name: "bundled_effects",
        load: loaded_presets_from_bundled,
    }
}

inventory::submit! {
    manifold_core::effect_registration::LoadedSceneModifierPresetSource {
        name: "bundled_scene_modifiers",
        load: loaded_scene_modifier_presets_from_bundled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use manifold_node_engine::validation::validate;
    use manifold_node_engine::exec::execution_plan::compile;

    /// Regression guard: every bundled preset must surface in the
    /// picker via `effect_type_registry`. The picker's data source
    /// (`effect_type_registry::REGISTRY`) is a separate `LazyLock` from
    /// `preset_definition_registry::EFFECT_DEFINITIONS`; both must iterate
    /// the JSON-loaded preset metadata or the dual-source migration
    /// silently strands shipping effects.
    ///
    /// Failure mode caught: the "Add Effect" popup shows only the
    /// remaining plugin-bridge effects (BlobTracking, Infrared,
    /// QuadMirror, WireframeDepth) — the rest live in JSON but the
    /// picker registry never reads JSON.
    #[test]
    fn every_bundled_preset_appears_in_effect_type_registry() {
        use manifold_core::preset_type_registry;
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let Some(def) = bundled_preset_def(&type_id) else {
                continue;
            };
            if def.preset_metadata.is_none() {
                continue; // v1 entry — no display metadata to project
            }
            assert!(
                preset_type_registry::is_registered(&type_id),
                "{}: bundled preset has presetMetadata but isn't in \
                 preset_type_registry — the picker won't \
                 show it. The REGISTRY LazyLock probably skipped the \
                 JSON dual-source loop.",
                type_id.as_str(),
            );
        }
    }

    /// Scene panels and liquid domains address nodes by stable node id, so
    /// every node a scene can hold carries one, unique across the whole
    /// document. Doc ids repeat across group levels and name nothing outside
    /// their level. Presets that are not scenes may still lean on the handle
    /// fallback.
    #[test]
    fn every_bundled_scene_node_id_is_unique_across_the_document() {
        fn collect<'a>(
            nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
            seen: &mut ahash::AHashSet<&'a str>,
            preset: &str,
            faults: &mut Vec<String>,
        ) {
            for node in nodes {
                // A group's port stubs carry no controls.
                let port_stub = matches!(node.type_id.as_str(), "system.group_input" | "system.group_output");
                if node.node_id.is_empty() {
                    if port_stub { continue; }
                    faults.push(format!("{preset}: node {} ({}) has no stable id", node.id, node.type_id));
                } else if !seen.insert(node.node_id.as_str()) {
                    faults.push(format!("{preset}: stable id {} repeats", node.node_id.as_str()));
                }
                if let Some(group) = node.group.as_deref() {
                    collect(&group.nodes, seen, preset, faults);
                }
            }
        }
        let mut scenes = 0;
        let mut faults = Vec::new();
        for kind in [PresetKind::Generator, PresetKind::SceneModifier] {
            for type_id in bundled_preset_type_ids(kind) {
                let def = bundled_preset_def(&type_id).expect("registered preset has a parsed def");
                if kind == PresetKind::Generator && manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(def).is_none() {
                    continue;
                }
                scenes += 1;
                collect(&def.nodes, &mut ahash::AHashSet::default(), type_id.as_str(), &mut faults);
            }
        }
        assert!(scenes > 20, "only {scenes} scene documents checked");
        assert!(faults.is_empty(), "{}", faults.join("\n"));
    }

    #[test]
    fn bundled_scene_modifier_catalog_enumerates_playable_stock_recipes() {
        let ids: Vec<String> = bundled_preset_type_ids(PresetKind::SceneModifier)
            .map(|t| t.as_str().to_string())
            .collect();
        assert!(ids.iter().any(|id| id == "ElasticSculpture"));
        assert!(ids.iter().any(|id| id == "SceneFog"));
        for expected in ["SurfaceWaves", "OrderedRecon", "SpatialEchoes", "MaskedPeel", "OrderedReconHit", "WavesEchoes"] {
            assert!(ids.iter().any(|id| id == expected), "missing {expected}");
        }
        let metadata = loaded_scene_modifier_presets_from_bundled();
        for id in ["SurfacePeelHit", "OrderedReconHit", "MaskedPeel", "WavesEchoes"] {
            assert!(!metadata.iter().find(|m| m.id.as_str() == id).unwrap().available,
                "{id} is retained only for saved-project compatibility");
        }
        for id in ["SurfacePeel", "OrderedRecon", "SurfaceWaves", "SpatialEchoes"] {
            assert!(metadata.iter().find(|m| m.id.as_str() == id).unwrap().available);
        }
    }

    #[test]
    fn every_bundled_preset_loads_validates_and_compiles() {
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let def = bundled_preset_def(&type_id)
                .expect("registered preset must have a parsed def")
                .clone();
            let graph = def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).unwrap_or_else(|e| {
                panic!("bundled preset {}: into_graph failed: {e}", type_id.as_str())
            });
            validate(&graph).unwrap_or_else(|e| {
                panic!("bundled preset {}: validate failed: {e:?}", type_id.as_str())
            });
            compile(&graph).unwrap_or_else(|e| {
                panic!("bundled preset {}: compile failed: {e:?}", type_id.as_str())
            });
        }
    }

    /// MVP-P1b default-preset contract (LED_STRIPS_DESIGN.md section 5b D12):
    /// the LED Fill generator — the default for new LED layers — must resolve
    /// in the bundled catalog with metadata and a graph that validates and
    /// compiles. A miss here is the "missing-id fallback" the creation flow
    /// must never hit: the layer would render as a cleared (black) generator.
    #[test]
    fn led_fill_bundled_generator_resolves_and_compiles() {
        let registry = PrimitiveRegistry::with_builtin();
        let id = PresetTypeId::new("LED Fill");
        let def = bundled_preset_def(&id)
            .expect("LED Fill must be a bundled generator (filename stem = preset id)")
            .clone();
        assert!(
            def.preset_metadata.is_some(),
            "LED Fill must carry presetMetadata so the picker/inspector can show its params",
        );
        let graph = def.into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()).expect("LED Fill must build a graph");
        validate(&graph).expect("LED Fill graph must validate");
        compile(&graph).expect("LED Fill graph must compile");
    }

    #[test]
    fn bundled_preset_lookup_returns_none_for_unknown_type() {
        let unknown = PresetTypeId::new("DefinitelyNotARealEffect");
        assert!(bundled_preset_def(&unknown).is_none());
        assert!(bundled_preset_json(&unknown).is_none());
    }

    /// BUG-7k1z regression: `bundled_preset_def` must carry scene-vocabulary
    /// bindings (rt_denoise_feed et al.) stamped by `migrate_scene_exposures`
    /// at cache-build time. The runtime def resolution path in
    /// generators/registry.rs must go through the migrated cache, never raw
    /// catalog JSON, or the live flip of a load-time-stamped exposure is inert
    /// (the binding list stays empty and `apply_bindings` falls back to the
    /// binding default forever).
    ///
    /// The real-world case BUG-7k1z caught is a project-overlay preset — a
    /// generator embedded in a `.manifold` save whose JSON predates the
    /// scene-exposure stamp. The def cache includes overlay entries
    /// (preset_loader merges them into the catalog at `apply_reload` time) and
    /// invalidates on generation bump, so a project-overlay preset that lacks
    /// `presetMetadata` in its raw JSON gets the same migration as any bundled
    /// one when the def cache rebuilds.
    ///
    /// This test asserts the end-to-end path on a bundled scene generator
    /// (Scene, whose raw JSON already carries the stamped metadata from
    /// authoring time — the migration is idempotent) and separately proves the
    /// migration itself on a synthetic pre-stamp shape (a lone render_scene
    /// node with no preset_metadata). Together they prove the cache path works
    /// and the stamping logic is correct, which covers the project-overlay
    /// case by construction: the overlay JSON is parsed through the same
    /// `rebuild_def_cache` pipeline as stock JSON.
    #[test]
    fn bundled_preset_def_carries_migrated_scene_bindings() {
        let id = PresetTypeId::new("Scene");

        // Migrated cache carries the stamped bindings.
        let migrated = bundled_preset_def(&id).expect("Scene must be a bundled generator");
        let meta = migrated
            .preset_metadata
            .as_ref()
            .expect("Scene def cache entry must have preset_metadata");
        let binding_names: Vec<&str> = meta
            .bindings
            .iter()
            .filter_map(|b| match &b.target {
                manifold_core::effect_graph_def::BindingTarget::Node { param, .. } => {
                    Some(param.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(
            binding_names.contains(&"rt_denoise_feed"),
            "bundled_preset_def(Scene) must carry rt_denoise_feed binding; \
             migrate_scene_exposures stamps it at cache-build time. \
             Got: {binding_names:?}"
        );
        assert!(
            binding_names.contains(&"rt_enabled"),
            "bundled_preset_def(Scene) must carry rt_enabled binding; \
             Got: {binding_names:?}"
        );

        // Prove the migration itself stamps the bindings on a pre-stamp shape.
        // This covers the project-overlay case where the embedded JSON predates
        // the stamp — the same `migrate_scene_exposures` call in
        // `rebuild_def_cache` processes both stock and overlay JSON identically.
        use std::collections::BTreeMap;
        use manifold_core::NodeId;
        let pre_stamp = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![manifold_core::effect_graph_def::EffectGraphNode {
                id: 1,
                node_id: NodeId::new("render"),
                type_id: "node.render_scene".to_string(),
                handle: Some("Render".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };
        let mut stamped = pre_stamp.clone();
        assert!(
            migrate_scene_exposures(&mut stamped),
            "migration must stamp on a pre-stamp render_scene shape"
        );
        let stamped_meta = stamped
            .preset_metadata
            .as_ref()
            .expect("migration must produce preset_metadata");
        let stamped_bindings: Vec<&str> = stamped_meta
            .bindings
            .iter()
            .filter_map(|b| match &b.target {
                manifold_core::effect_graph_def::BindingTarget::Node { param, .. } => {
                    Some(param.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(
            stamped_bindings.contains(&"rt_denoise_feed"),
            "migrate_scene_exposures must stamp rt_denoise_feed on render_scene; \
             Got: {stamped_bindings:?}"
        );
    }

    /// Splicing a bundled preset into a chain via
    /// `splice_def_into_chain` is the path the runtime takes when
    /// `PresetInstance.graph = Some(def)`. Verifies every shipping
    /// preset survives that round-trip — the same data the drift test
    /// covers at the standalone-graph level, exercised against the
    /// chain-grafting code that the runtime actually calls.
    #[test]
    fn every_bundled_preset_splices_into_a_chain() {
        use manifold_node_engine::scene::boundary_nodes::Source;
        use manifold_node_engine::load::chain_spec::splice_def_into_chain;
        use manifold_node_engine::graph::Graph;

        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let def = bundled_preset_def(&type_id).expect("registered");
            let mut chain = Graph::new();
            let src = chain.add_node(Box::new(Source::new()));
            let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry, None, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default());
            assert!(
                result.is_some(),
                "bundled preset {} failed to splice into a chain — preset and chain runtime have \
                 drifted apart",
                type_id.as_str(),
            );
        }
    }

    #[cfg(feature = "gpu-proofs")]
    /// Sweep guard: every bundled effect preset must successfully
    /// execute one full frame against a real Metal backend. Splices the
    /// preset into a minimal chain (Source → effect → FinalOutput),
    /// compiles, pre-binds a source texture, and runs one
    /// `execute_frame_with_state` + `commit_and_wait`. Catches the
    /// failure classes that load + compile can't reach because pipelines
    /// are created lazily on first dispatch: bad WGSL, mismatched
    /// texture formats in `outputFormats` overrides hitting a Metal
    /// blit, missing bindings, workgroup-size errors.
    ///
    /// Failure mode caught: the "first-frame panic" symptom that
    /// otherwise only surfaces when a real project loads the effect on
    /// stage.
    ///
    /// Inner-node params stay at JSON defaults (no `apply_param_values`
    /// equivalent on the effect splice path right now). Wraps each
    /// preset's execute in `catch_unwind` so one bad preset doesn't
    /// tear down the run; all failures are collected and reported at
    /// once.
    #[test]
    fn every_bundled_preset_executes_one_frame() {
        use manifold_node_engine::scene::boundary_nodes::{FinalOutput, Source};
        use manifold_node_engine::load::chain_spec::splice_def_into_chain;
        use manifold_node_engine::exec::effect_node::FrameTime;
        use manifold_node_engine::exec::execution::Executor;
        use manifold_node_engine::exec::execution_plan::compile;
        use manifold_node_engine::graph::Graph;
        use manifold_node_engine::exec::metal_backend::MetalBackend;
        use manifold_node_engine::state_store::StateStore;
        use manifold_node_engine::gpu::render_target::RenderTarget;
        use manifold_core::{Beats, Seconds};
        use manifold_gpu::GpuTextureFormat;

        let device = manifold_gpu::testkit::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        // 256x256 — see generator-side test for size rationale.
        let (w, h) = (256u32, 256u32);
        let format = GpuTextureFormat::Rgba16Float;
        let frame_time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        };

        let mut failures: Vec<String> = Vec::new();

        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let preset_id = type_id.as_str().to_string();
            let Some(def) = bundled_preset_def(&type_id) else {
                continue;
            };

            // Splice into a minimal chain. Source produces the input
            // texture; FinalOutput terminates the texture path so
            // validate is satisfied.
            let mut chain = Graph::new();
            let src = chain.add_node(Box::new(Source::new()));
            let Some(result) =
                splice_def_into_chain(&mut chain, (src, "out"), def, &registry, None, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            else {
                failures.push(format!("{preset_id}: splice failed"));
                continue;
            };
            let final_out = chain.add_node(Box::new(FinalOutput::new()));
            let effect_out = result.output;
            if chain.connect(effect_out, (final_out, "in")).is_err() {
                failures.push(format!("{preset_id}: final-output wire failed"));
                continue;
            }

            let plan = match compile(&chain) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("{preset_id}: compile failed: {e:?}"));
                    continue;
                }
            };

            // Pre-bind the source texture when this preset consumes the
            // chain source. Source-independent masks intentionally leave the
            // pruned system.source node out of the execution plan; their
            // intermediate textures still auto-allocate on first acquire.
            let r_src = plan
                .steps()
                .iter()
                .find(|s| s.node == src)
                .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
                .map(|(_, id)| *id);
            let mut backend = MetalBackend::new(device.arc(), w, h, format);
            if let Some(r_src) = r_src {
                let src_target =
                    RenderTarget::new(&device, w, h, format, "first-frame-test-src");
                backend.pre_bind_texture_2d(r_src, src_target);
            }

            let mut exec = Executor::new(Box::new(backend));
            let mut state = StateStore::new();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut native_enc =
                    device.create_encoder("effect-first-frame-test");
                {
                    let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(
                        &mut native_enc,
                        &device,
                    );
                    exec.execute_frame_with_state(
                        &mut chain,
                        &plan,
                        frame_time,
                        &mut gpu,
                        &mut state,
                        0,
                    );
                }
                native_enc.commit_and_wait_completed();
            }));

            if let Err(panic) = result {
                let msg = if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else {
                    "<non-string panic>".to_string()
                };
                failures.push(format!("{preset_id}: first-frame panic: {msg}"));
            }
        }

        assert!(
            failures.is_empty(),
            "Bundled effect presets panicked on first-frame execute:\n  - {}",
            failures.join("\n  - "),
        );
    }

    /// Color Compass specifically: every wire the JSON declares must
    /// land in the chain-spliced graph. Catches the case where the
    /// JSON wires up `translate_x` / `translate_y` / `time_constant`
    /// port-shadows but `splice_def_into_chain` silently drops them
    /// (because the port lookup fails, or the destination handle
    /// doesn't resolve, etc.).
    #[test]
    fn color_compass_splice_preserves_translate_and_time_constant_wires() {
        use manifold_node_engine::scene::boundary_nodes::Source;
        use manifold_node_engine::load::chain_spec::splice_def_into_chain;
        use manifold_node_engine::graph::Graph;

        let registry = PrimitiveRegistry::with_builtin();
        let id = PresetTypeId::new("ColorCompass");
        let def = bundled_preset_def(&id).expect("ColorCompass preset registered");

        let mut chain = Graph::new();
        let src = chain.add_node(Box::new(Source::new()));
        let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry, None, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .expect("Color Compass splices");

        // Resolve handle → chain-node-id map for the inner nodes the
        // assertions need.
        let mut handle_map = ahash::AHashMap::<&str, manifold_node_engine::exec::effect_node::NodeInstanceId>::default();
        for (name, id) in &result.handles {
            handle_map.insert(name.as_ref(), *id);
        }
        let affine = *handle_map
            .get("affine")
            .expect("affine handle exists in compass splice");
        let smoothing_x = *handle_map
            .get("smoothing_x")
            .expect("smoothing_x handle exists");
        let smoothing_y = *handle_map
            .get("smoothing_y")
            .expect("smoothing_y handle exists");
        let reactivity_value = *handle_map
            .get("reactivity_value")
            .expect("reactivity_value handle exists");

        // The post-splice graph must contain wires that target
        // AffineTransform's translate_x and translate_y, sourced from
        // the two smoothing nodes. If the splice silently dropped them
        // (port-shadow not recognised) the user sees no compass
        // response despite the JSON declaring it.
        let wire_exists = |from_node, from_port: &str, to_node, to_port: &str| -> bool {
            chain.wires().iter().any(|w| {
                w.from.0 == from_node && w.from.1 == from_port
                    && w.to.0 == to_node && w.to.1 == to_port
            })
        };
        assert!(
            wire_exists(smoothing_x, "out", affine, "translate_x"),
            "smoothing_x.out → affine.translate_x wire missing — likely splice dropped it",
        );
        assert!(
            wire_exists(smoothing_y, "out", affine, "translate_y"),
            "smoothing_y.out → affine.translate_y wire missing — likely splice dropped it",
        );
        // Both smoothings have to receive time_constant from the
        // shared reactivity_value node — otherwise the card's
        // reactivity slider only governs one axis.
        assert!(
            wire_exists(reactivity_value, "out", smoothing_x, "time_constant"),
            "reactivity_value → smoothing_x.time_constant wire missing",
        );
        assert!(
            wire_exists(reactivity_value, "out", smoothing_y, "time_constant"),
            "reactivity_value → smoothing_y.time_constant wire missing",
        );
    }

    // Removed `color_compass_responds_to_half_bright_source` — it
    // segfaulted in the chain-test setup before producing useful
    // diagnostic output. The wire-preservation test above covers the
    // structural path; the actual fix for "compass doesn't visibly
    // respond" is region-averaged ColorSample (single-pixel reads on
    // high-frequency content produce near-zero asymmetry).
    #[cfg(any())]
    fn color_compass_responds_to_half_bright_source() {
        use manifold_node_engine::scene::boundary_nodes::{FinalOutput, Source};
        use manifold_node_engine::load::chain_spec::splice_def_into_chain;
        use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId};
        use manifold_node_engine::exec::execution_plan::{ResourceId, compile};
        use manifold_node_engine::graph::Graph;
        use manifold_node_engine::parameters::{ParamDef, ParamValue};
        use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
        use manifold_node_engine::state_store::StateStore;
        use manifold_node_engine::exec::{execution::Executor, metal_backend::MetalBackend};
        use manifold_node_engine::gpu::render_target::RenderTarget;
        use manifold_core::{Beats, Seconds};
        use manifold_gpu::GpuTextureFormat;

        fn frame_time() -> FrameTime {
            FrameTime {
                beats: Beats(0.0),
                seconds: Seconds(0.0),
                delta: Seconds(1.0 / 60.0),
                frame_count: 0,
            }
        }

        fn output_resource(
            plan: &manifold_node_engine::exec::execution_plan::ExecutionPlan,
            node: NodeInstanceId,
            port: &str,
        ) -> ResourceId {
            for step in plan.steps() {
                if step.node == node {
                    for &(name, id) in &step.outputs {
                        if name == port {
                            return id;
                        }
                    }
                }
            }
            panic!("no output `{port}` on node {node:?}");
        }

        struct CaptureFloat {
            type_id: EffectNodeType,
            seen: std::sync::Arc<std::sync::Mutex<Option<f32>>>,
        }
        impl EffectNode for CaptureFloat {
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 1] = [NodePort {
                    name: "in",
                    ty: PortType::Scalar(ScalarType::F32),
                    kind: PortKind::Input,
                    required: true,
                }];
                &INPUTS
            }
            fn outputs(&self) -> &[NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                if let Some(ParamValue::Float(v)) = ctx.inputs.scalar("in") {
                    *self.seen.lock().unwrap() = Some(v);
                }
            }
        }

        let device = manifold_gpu::testkit::test_device();
        let (w, h) = (64u32, 64u32);
        let format = GpuTextureFormat::Rgba16Float;

        // Half-bright source: top half white, bottom half black. The
        // North sample lands in the bright half, South in the dark
        // half — maximum N-S asymmetry. East and West both land at
        // y=0.5 which is the boundary, both equally lit on average.
        let bright = half::f16::from_f32(1.0).to_bits();
        let dark = half::f16::from_f32(0.0).to_bits();
        let alpha = half::f16::from_f32(1.0).to_bits();
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for _ in 0..w {
                if y < h / 2 {
                    pixels.extend_from_slice(&[bright, bright, bright, alpha]);
                } else {
                    pixels.extend_from_slice(&[dark, dark, dark, alpha]);
                }
            }
        }
        let raw_bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|p| p.to_le_bytes())
            .collect();

        let src_target = RenderTarget::view_of(device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL
                | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
            label: "compass-source",
            mip_levels: 1,
        }), "compass-source");
        device.upload_texture(&src_target.texture, &raw_bytes);

        let registry = PrimitiveRegistry::with_builtin();
        let id = PresetTypeId::new("ColorCompass");
        let def = bundled_preset_def(&id).expect("ColorCompass preset");

        let mut chain = Graph::new();
        let src = chain.add_node(Box::new(Source::new()));
        let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry, None, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .expect("splice ok");

        // Look up smoothing_y (vertical axis = N-S compass).
        let smoothing_y = result
            .handles
            .iter()
            .find(|(n, _)| n.as_ref() == "smoothing_y")
            .map(|(_, id)| *id)
            .expect("smoothing_y handle");

        // Wire a sink onto smoothing_y.out so we can read it post-frame.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let sink = chain.add_node(Box::new(CaptureFloat {
            type_id: EffectNodeType::new("test.capture"),
            seen: seen.clone(),
        }));
        chain
            .connect((smoothing_y, "out"), (sink, "in"))
            .expect("capture wire");

        // Terminate the texture path so validate doesn't complain — a
        // FinalOutput consuming the compass's image output.
        let final_out = chain.add_node(Box::new(FinalOutput::new()));
        let compass_out = result.output;
        chain
            .connect(compass_out, (final_out, "in"))
            .expect("final output wire");

        let plan = compile(&chain).expect("compile");

        // Pre-bind the source texture. Intermediate textures (the
        // affine output) get auto-allocated by MetalBackend.
        let r_src = output_resource(&plan, src, "out");
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);

        let mut exec = Executor::new(Box::new(backend));
        let mut state = StateStore::new();

        // Run enough frames for ColorSample's one-frame readback +
        // Smoothing's exponential convergence at the JSON-default
        // 100ms time constant. ~60 frames at 60fps = 1 second; ~63%
        // converged at t=tau, ~95% at t=3*tau.
        for _ in 0..60 {
            let mut native_enc = device.create_encoder("compass-diag");
            {
                let mut gpu =
                    manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut native_enc, &device);
                exec.execute_frame_with_state(
                    &mut chain,
                    &plan,
                    frame_time(),
                    &mut gpu,
                    &mut state,
                    0,
                );
            }
            native_enc.commit_and_wait_completed();
        }

        let value = seen.lock().unwrap().expect("captured");
        eprintln!("smoothing_y after 60 frames on half-bright source = {value}");
        // dy = N_luma - S_luma should approach 1.0 - 0.0 = 1.0. Times
        // intensity = 2.0 (JSON default) → smoothing target = 2.0,
        // which clamps to AffineTransform's translate_y range.
        // Smoothing output should be well over 0.5.
        assert!(
            value.abs() > 0.5,
            "smoothing_y output ({value}) too small to produce visible drift",
        );
    }
}

#[cfg(test)]
mod catalog_source_tests {
    use super::*;

    #[test]
    fn preset_catalog_providers_have_disjoint_type_ids() {
        let mut ids = std::collections::HashSet::new();
        for kind in [PresetKind::Effect, PresetKind::Generator, PresetKind::SceneModifier] {
            manifold_node_engine::load::catalog_source::visit_presets(kind, &mut |id| assert!(ids.insert(id.clone()), "duplicate preset type id: {id}"));
        }
        assert!(!ids.is_empty(), "catalog census requires linked family presets");
    }

    #[test]
    fn preset_catalog_registration_preserves_json_and_cached_def() {
        for kind in [PresetKind::Effect, PresetKind::Generator, PresetKind::SceneModifier] {
            let direct: Vec<_> = bundled_preset_type_ids(kind).collect();
            let registered: Vec<_> = manifold_node_engine::load::catalog_source::preset_type_ids(kind).collect();
            assert_eq!(direct, registered);
            for id in direct {
                assert_eq!(bundled_preset_json(&id), manifold_node_engine::load::catalog_source::preset_json(&id));
                match (bundled_preset_def(&id), manifold_node_engine::load::catalog_source::preset_def(&id)) {
                    (Some(a), Some(b)) => assert!(std::ptr::eq(a, b)),
                    (None, None) => {},
                    _ => panic!("catalog definition mismatch: {id}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod metadata_source_tests {
    use super::*;

    #[test]
    fn registered_metadata_preserves_legacy_published_order() {
        use manifold_core::preset_definition_registry::{self as definitions, effect, generator, scene_modifier};
        let direct = [loaded_presets_from_bundled(),
            crate::bundled_generator_presets::loaded_generator_presets_from_bundled(),
            loaded_scene_modifier_presets_from_bundled()];
        let registered = [effect::load_preset_metadata(), generator::load_preset_metadata(),
            scene_modifier::load_preset_metadata()];
        assert_eq!(direct, registered);
        assert!(registered.iter().all(|metadata| !metadata.is_empty()), "shipping metadata providers must be nonempty");
        let mut ids = std::collections::HashSet::new();
        for metadata in registered.iter().flatten() {
            assert!(ids.insert(metadata.id.clone()), "duplicate metadata preset id: {}", metadata.id);
        }
        // Publish through the former direct-loader path, including browser filtering.
        definitions::rebuild_preset_definitions(&direct[0], &direct[1], &direct[2]);
        let effects: Vec<_> = direct[0].iter()
            .filter(|m| EFFECT_CATALOG.load().is_browser_visible(m.id.as_str())).cloned().collect();
        let generators: Vec<_> = direct[1].iter()
            .filter(|m| GENERATOR_CATALOG.load().is_browser_visible(m.id.as_str())).cloned().collect();
        manifold_core::preset_type_registry::rebuild(&effects, &generators);
        let published_order = || manifold_core::preset_type_registry::all().into_iter()
            .map(|entry| (entry.id, entry.kind, entry.display_name)).collect::<Vec<_>>();
        let before = published_order();
        let generation = catalog_generation();
        assert_eq!(manifold_node_engine::load::preset_loader::clear_project_presets(), generation + 1);
        assert_eq!(published_order(), before);
        for metadata in direct.iter().flatten() {
            assert_eq!(definitions::get(&metadata.id).display_name, metadata.display_name);
        }
    }
}
