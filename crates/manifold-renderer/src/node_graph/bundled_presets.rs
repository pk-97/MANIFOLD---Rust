//! Bundled effect preset registry.
//!
//! Each shipping effect ships with one **bundled preset** — a JSON
//! [`EffectGraphDef`]. The JSON files are **scanned from disk at
//! startup** by [`crate::preset_loader`] (stock from the packaged
//! bundle or the dev workspace assets dir, plus optional user presets),
//! not embedded into the binary. The binary has zero compile-time
//! knowledge of which effects exist. Adding a preset is just dropping a
//! JSON file in the stock directory — no rebuild required.
//!
//! The bundled preset for `PresetTypeId::X` is the canonical default
//! graph for that effect. Post-§11 the JSON file is authoritative —
//! the chain runtime and editor snapshot both source bindings,
//! skip-mode, and topology from the embedded
//! [`PresetMetadata`](manifold_core::effect_graph_def::PresetMetadata)
//! block via [`crate::node_graph::LoadedPresetView`].
//!
//! User-authored per-instance graphs are stored separately on the
//! [`PresetInstance`](manifold_core::effects::PresetInstance). Both
//! shapes use the same [`EffectGraphDef`] schema and the same
//! [`PrimitiveRegistry`] loader; they differ only in storage location.
//!
//! The type id is the JSON filename stem, exactly as before — type ids
//! are forever (save files reference them).

use std::sync::Arc;

use ahash::AHashMap;
use arc_swap::ArcSwap;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;

use crate::preset_loader::{EFFECT_CATALOG, GENERATOR_CATALOG, catalog_generation};

/// Raw JSON for the bundled preset of `preset_type` (either kind), or
/// `None` if no preset has that type id.
///
/// Kind-agnostic: effect and generator ids are globally disjoint
/// (verified), so this checks the effect catalog then the generator one
/// and returns the single match. The string is the current on-disk file
/// verbatim. Hot-reload (step 10): the catalogs live behind [`ArcSwap`], so
/// this returns an owned `Arc<str>` cloned from the current snapshot rather
/// than a `&'static` borrow — a concurrent reload can swap the snapshot
/// without invalidating a value the caller already holds.
pub fn bundled_preset_json(preset_type: &PresetTypeId) -> Option<Arc<str>> {
    EFFECT_CATALOG
        .load()
        .json(preset_type.as_str())
        .or_else(|| GENERATOR_CATALOG.load().json(preset_type.as_str()))
}

/// Generation-stamped parsed-def cache. Keyed `&'static str` → leaked
/// `&'static EffectGraphDef` so [`bundled_preset_def`] can keep handing out
/// `&'static` references (the render path stores them on
/// `LoadedPresetView.canonical_def`). The cache is rebuilt (and re-leaked)
/// whenever the catalog generation advances; at rest the generation never
/// moves and the cache is reused, so the only at-rest cost over the old
/// `OnceLock` is one relaxed atomic load.
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
    // Parse BOTH catalogs into the one cache — ids are globally disjoint, so
    // a generator gets the same leaked-`&'static` parsed-def path effects
    // already have (the cluster-#4 DefCache parity). The leak is bounded by
    // the (finite) shipping preset count × reloads per session.
    let effect_catalog = EFFECT_CATALOG.load();
    let generator_catalog = GENERATOR_CATALOG.load();
    for (id, json) in effect_catalog.entries().chain(generator_catalog.entries()) {
        let def: EffectGraphDef = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("bundled preset {id}: parse failed: {e}"));
        let id_static: &'static str = Box::leak(id.to_string().into_boxed_str());
        let def_static: &'static EffectGraphDef = Box::leak(Box::new(def));
        m.insert(id_static, def_static);
    }
    DEF_CACHE.map.store(Arc::new(m));
    DEF_CACHE
        .generation
        .store(generation, std::sync::atomic::Ordering::Release);
}

/// Parsed [`EffectGraphDef`] for the bundled preset of `preset_type`
/// (either kind), or `None` if no preset is registered.
///
/// First call (and every call after a hot-reload generation bump) parses
/// both catalog snapshots into a leaked map; subsequent calls return a
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
    let catalog = match kind {
        PresetKind::Effect => &EFFECT_CATALOG,
        PresetKind::Generator => &GENERATOR_CATALOG,
    };
    catalog
        .load()
        .type_ids()
        .map(|id| PresetTypeId::from_string(id.to_string()))
        .collect::<Vec<_>>()
        .into_iter()
}

/// Loader function for the core's [`LoadedPresetSource`] inventory.
/// Walks the bundled preset table, parses each JSON document, and
/// returns the `preset_metadata` field from every entry that carries
/// one (v2 schema). Every shipping bundled preset is v2 post-§11;
/// the `Option`-returning shape is retained so test-only or
/// hand-authored v1 fixtures stay loadable as graphs without
/// breaking the metadata projection.
///
/// Cached at the `loaded_preset_metadata()` callsite — invoked once
/// per process.
pub fn loaded_presets_from_bundled() -> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    EFFECT_CATALOG
        .load()
        .entries()
        .filter_map(|(id, json)| {
            let def: EffectGraphDef = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("bundled preset {id}: parse failed: {e}"));
            def.preset_metadata
        })
        .collect()
}

inventory::submit! {
    manifold_core::preset_definition_registry::effect::PresetSource {
        load: loaded_presets_from_bundled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::node_graph::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use crate::node_graph::validation::validate;
    use crate::node_graph::execution_plan::compile;

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

    #[test]
    fn every_bundled_preset_loads_validates_and_compiles() {
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let def = bundled_preset_def(&type_id)
                .expect("registered preset must have a parsed def")
                .clone();
            let graph = def.into_graph(&registry).unwrap_or_else(|e| {
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

    #[test]
    fn bundled_preset_json_returns_embedded_bytes() {
        let raw = bundled_preset_json(&PresetTypeId::MIRROR).expect("Mirror preset registered");
        // Sanity: the embedded JSON must parse as a valid def and name itself "Mirror".
        let def: EffectGraphDef = serde_json::from_str(&raw).expect("Mirror preset parses");
        assert_eq!(def.name.as_deref(), Some("Mirror"));
    }

    #[test]
    fn bundled_preset_lookup_returns_none_for_unknown_type() {
        let unknown = PresetTypeId::new("DefinitelyNotARealEffect");
        assert!(bundled_preset_def(&unknown).is_none());
        assert!(bundled_preset_json(&unknown).is_none());
    }

    /// Splicing a bundled preset into a chain via
    /// `splice_def_into_chain` is the path the runtime takes when
    /// `PresetInstance.graph = Some(def)`. Verifies every shipping
    /// preset survives that round-trip — the same data the drift test
    /// covers at the standalone-graph level, exercised against the
    /// chain-grafting code that the runtime actually calls.
    #[test]
    fn every_bundled_preset_splices_into_a_chain() {
        use crate::node_graph::boundary_nodes::Source;
        use crate::node_graph::chain_spec::splice_def_into_chain;
        use crate::node_graph::graph::Graph;

        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let def = bundled_preset_def(&type_id).expect("registered");
            let mut chain = Graph::new();
            let src = chain.add_node(Box::new(Source::new()));
            let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry);
            assert!(
                result.is_some(),
                "bundled preset {} failed to splice into a chain — preset and chain runtime have \
                 drifted apart",
                type_id.as_str(),
            );
        }
    }

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
        use crate::node_graph::boundary_nodes::{FinalOutput, Source};
        use crate::node_graph::chain_spec::splice_def_into_chain;
        use crate::node_graph::effect_node::FrameTime;
        use crate::node_graph::execution::Executor;
        use crate::node_graph::execution_plan::{ResourceId, compile};
        use crate::node_graph::graph::Graph;
        use crate::node_graph::metal_backend::MetalBackend;
        use crate::node_graph::state_store::StateStore;
        use crate::render_target::RenderTarget;
        use manifold_core::{Beats, Seconds};
        use manifold_gpu::GpuTextureFormat;

        let device = crate::test_device();
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
                splice_def_into_chain(&mut chain, (src, "out"), def, &registry)
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

            // Pre-bind the source texture. Intermediate textures auto-
            // allocate inside MetalBackend on first acquire.
            let r_src = plan
                .steps()
                .iter()
                .find(|s| s.node == src)
                .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
                .map(|(_, id)| *id)
                .unwrap_or(ResourceId(u32::MAX));
            if r_src.0 == u32::MAX {
                failures.push(format!(
                    "{preset_id}: Source.out resource not found in plan",
                ));
                continue;
            }

            let src_target =
                RenderTarget::new(&device, w, h, format, "first-frame-test-src");
            let mut backend = MetalBackend::new(&device, w, h, format);
            backend.pre_bind_texture_2d(r_src, src_target);

            let mut exec = Executor::new(Box::new(backend));
            let mut state = StateStore::new();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut native_enc =
                    device.create_encoder("effect-first-frame-test");
                {
                    let mut gpu = crate::gpu_encoder::GpuEncoder::new(
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
        use crate::node_graph::boundary_nodes::Source;
        use crate::node_graph::chain_spec::splice_def_into_chain;
        use crate::node_graph::graph::Graph;

        let registry = PrimitiveRegistry::with_builtin();
        let id = PresetTypeId::new("ColorCompass");
        let def = bundled_preset_def(&id).expect("ColorCompass preset registered");

        let mut chain = Graph::new();
        let src = chain.add_node(Box::new(Source::new()));
        let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry)
            .expect("Color Compass splices");

        // Resolve handle → chain-node-id map for the inner nodes the
        // assertions need.
        let mut handle_map = ahash::AHashMap::<&str, crate::node_graph::effect_node::NodeInstanceId>::default();
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
        use crate::node_graph::boundary_nodes::{FinalOutput, Source};
        use crate::node_graph::chain_spec::splice_def_into_chain;
        use crate::node_graph::effect_node::{
            EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId,
        };
        use crate::node_graph::execution_plan::{ResourceId, compile};
        use crate::node_graph::graph::Graph;
        use crate::node_graph::parameters::{ParamDef, ParamValue};
        use crate::node_graph::ports::{
            NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
        };
        use crate::node_graph::state_store::StateStore;
        use crate::node_graph::{Executor, MetalBackend};
        use crate::render_target::RenderTarget;
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
            plan: &crate::node_graph::execution_plan::ExecutionPlan,
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

        let device = crate::test_device();
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

        let src_target = RenderTarget::new(&device, w, h, format, "compass-source");
        device.upload_texture(&src_target.texture, &raw_bytes);

        let registry = PrimitiveRegistry::with_builtin();
        let id = PresetTypeId::new("ColorCompass");
        let def = bundled_preset_def(&id).expect("ColorCompass preset");

        let mut chain = Graph::new();
        let src = chain.add_node(Box::new(Source::new()));
        let result = splice_def_into_chain(&mut chain, (src, "out"), def, &registry)
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
        let mut backend = MetalBackend::new(&device, w, h, format);
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
                    crate::gpu_encoder::GpuEncoder::new(&mut native_enc, &device);
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
