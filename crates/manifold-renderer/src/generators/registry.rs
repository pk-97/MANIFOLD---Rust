use crate::node_graph::{bundled_preset_json, bundled_preset_type_ids};
use crate::node_graph::primitives::{
    GltfTextureSource, RenderScene, ScatterOnMesh, SeedParticlesFromTexture,
};
use crate::preset_runtime::PresetRuntime;
use manifold_core::effects::RelightParams;
use manifold_core::preset_def::PresetKind;
use crate::node_graph::PrimitiveRegistry;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

/// Factory that maps PresetTypeId to concrete [`PresetRuntime`]
/// instances. Pipeline compilation happens at creation time (expensive — do at
/// startup or first use).
///
/// Every generator is a **bundled JSON preset** at
/// `assets/generator-presets/*.json`, embedded by `build.rs`; each becomes a
/// [`PresetRuntime`]. The legacy Rust-factory path (one `Generator` trait
/// impl per generator, registered via `inventory::submit!`) is gone — the
/// migration to JSON atom graphs is complete, so there is one concrete runtime.
pub struct GeneratorRegistry {
    target_format: GpuTextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: GpuTextureFormat) -> Self {
        Self { target_format }
    }

    /// Pre-compile all generator pipelines into the binary archive.
    /// Creates and immediately drops each generator — the compiled Metal pipeline
    /// binaries persist in the archive. Call at startup before `save_pipeline_archive()`.
    pub fn prewarm_all(&self, device: &std::sync::Arc<GpuDevice>) {
        let json_count = bundled_preset_type_ids(PresetKind::Generator).count();
        log::info!("Pre-warming {json_count} JSON generator pipelines...");
        // Pre-warm JSON-defined generators. We need a default
        // render resolution here — use a small placeholder; real sizes
        // come through on the first frame's `resize`. The pipelines
        // baked into each primitive cache at first dispatch regardless.
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_preset_type_ids(PresetKind::Generator) {
            if let Some(json) = bundled_preset_json(&type_id)
                && let Err(e) = PresetRuntime::from_json_str_with_device(
                    &json,
                    &registry,
                    std::sync::Arc::clone(device),
                    256,
                    256,
                    self.target_format,
                    None,
                )
            {
                log::warn!(
                    "Pre-warm of bundled generator preset {} failed: {e}",
                    type_id.as_str(),
                );
            }
        }

        // BUG-037: `node.render_scene` and `node.gltf_texture_source` compile
        // their GPU pipelines lazily inside `run()` (a hand-written
        // `Option::get_or_insert_with`, not the codegen-path pipelines the
        // bundled-preset loop above touches indirectly), gated on real
        // project data (a material actually drawn, a texture actually
        // decoded) that no bundled preset carries — so the loop above never
        // reaches them. Neither pipeline depends on project/asset content
        // (fixed shader source, fixed entry points), so both warm
        // unconditionally here rather than waiting for a real glTF scene
        // layer's first rendered frame to pay the compile cost
        // (`RENDER_TRACE` showed `generators=37.1ms` on that frame).
        RenderScene::prewarm_pipelines(device);
        GltfTextureSource::prewarm_pipeline(device);
        // BUG-037:
        // `node.scatter_on_mesh` is a barriered multi-pass scan/reduce
        // (exempt from the codegen path), so `prewarm_all_atom_codegen_pipelines`
        // below never reaches its three hand-written pipelines either. Same
        // asset-independent-fixed-source shape as the two lines above.
        ScatterOnMesh::prewarm_pipelines(device);
        // BUG-191: `node.spawn_from_image` is the same
        // barriered-multi-pass, exempt-from-codegen shape as
        // `scatter_on_mesh` above, and no bundled preset happens to
        // reference it either — its four hand-written pipelines were never
        // reached by any prewarm mechanism, so a project's first live use
        // (e.g. a clip becoming active for the first time after a
        // mid-timeline seek) paid the full compile cost on that frame.
        SeedParticlesFromTexture::prewarm_pipelines(device);

        // BUG-146: the two mechanisms above only reach atoms a BUNDLED
        // preset's *structure* happens to reference (the loop above never
        // calls `run()`) plus RenderScene/GltfTextureSource's two hand-listed
        // hand-written pipelines. Every atom on the `primitive!` codegen path
        // — now most atoms, after this session's wave 2-4 conversions —
        // compiles its GPU pipeline lazily on its OWN first `run()` via
        // `self.pipeline.get_or_insert_with(standalone_for_spec::<Self>())`,
        // untouched by either mechanism until some project's live first
        // frame happens to hit it. `node.cube_mesh` is the confirmed example
        // named in the backlog root cause (its doc comment names
        // DigitalPlants/NestedCubes as the intended consumer, though neither
        // bundled preset's JSON wires it in yet — it's a decomposition
        // building block, not currently reachable via any shipped preset's
        // structure, so the bundled-preset loop above could never warm it
        // even indirectly). The ~41.5ms residual BUG-145 measured is a
        // SEPARATE scene (2 occluders + 4 lights, shadows/shafts off) hitting
        // this same class of gap via other codegen-path atoms in that scene's
        // graph, not cube_mesh specifically — the mechanism generalizes to
        // any atom in this state, which is exactly why the fix below is
        // structural (sweep every registered atom) rather than naming one.
        // Measured directly (fresh, uncached `GpuDevice`, this session):
        // `node.cube_mesh` alone compiles cold in ~12-15ms vs ~0.02-0.04ms
        // once this sweep has run; the worst case of touching every one of
        // the ~144 codegen-path atoms cold in one frame (the shape of the
        // original BUG-037/145/146 diagnosis) sums to ~1.0-1.1s vs ~1-2ms
        // prewarmed. Fixed structurally, not atom-by-atom: sweep every
        // registered primitive type_id, construct it, and compile its
        // standalone kernel via `codegen::standalone_for_node` — the dynamic
        // (type-erased) mirror of `standalone_for_spec` (see its doc comment
        // for why this is possible without a per-atom hook: the blanket
        // `EffectNode` impl already forwards every const `standalone_for_spec`
        // needs through `&dyn EffectNode` methods). O(atom count), no GPU
        // inputs/fixtures required — `standalone_for_spec` only needs WGSL
        // text + a `PrimitiveSpec`'s consts, never bound resources.
        prewarm_all_atom_codegen_pipelines(device);

        log::info!("Generator pipeline pre-warm complete");
    }

    /// Create a new generator instance for the given type at the
    /// host's current canvas resolution. JSON presets are consulted
    /// first; falls back to Rust factories.
    ///
    /// `width`/`height` are the live canvas dims — passed straight
    /// into the JSON chain build so `canvas_sized_array_outputs`
    /// (scatter accumulators, density grids) allocate at the right
    /// pixel count on the very first construction. Callers must
    /// always pass the real canvas size; there is intentionally no
    /// fallback default — a hardcoded 1920×1080 here was the source
    /// of the "Strange Attractor renders in the top-left quadrant
    /// after generator swap" bug (the swap path constructed at the
    /// default and never called `resize`, leaving the splat buffer
    /// sized for a sub-rect of the real canvas).
    pub fn create(
        &self,
        device: std::sync::Arc<GpuDevice>,
        gen_type: &manifold_core::PresetTypeId,
        width: u32,
        height: u32,
    ) -> Option<Box<PresetRuntime>> {
        // No override, no watch context (perf-gate tuning / tests / non-editor
        // call sites) — fuse normally per the device verdict. No instance
        // manifest in scope here, so the reshape reads the def's own shadow.
        self.create_with_override(device, gen_type, None, width, height, false, None, None)
    }

    /// Same as [`Self::create`] but routes a per-layer
    /// `EffectGraphDef` override (from `Layer::generator_graph`)
    /// straight into [`PresetRuntime::from_def_with_device`].
    /// `override_def = None` falls back to the bundled JSON preset.
    ///
    /// `manifest` is the live per-instance [`ParamManifest`]
    /// (`Layer.gen_params.params`) on a project-generator rebuild, threaded
    /// straight into `from_def_with_device` so the reshape sources each
    /// param's range/curve/invert from the manifest authority instead of the
    /// graph's stale `preset_metadata.params` shadow (BUG-078). `None` for
    /// non-instance callers ([`Self::create`], perf-gate tuning) — those read
    /// the def's own shadow, which is accurate for a fresh-from-disk def.
    ///
    /// Returns `None` if neither the override nor the bundled preset
    /// can be loaded.
    ///
    /// `relight` is the "3D Shading" toggle at the compile level
    /// (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5): `Some(params)` passes the
    /// effective def through [`crate::node_graph::relight::relight_augment`]
    /// before `from_def_with_device` — the depth-companion synthesis + fixed
    /// relight template (parameterized by the instance's live knobs)
    /// appended before `final_output`. `None` is the exact unaugmented def,
    /// byte-identical to pre-P3 behavior. Also vetoes whole-generator fusion
    /// for this build (see below) — a fused kernel has no topology left for
    /// the template to splice onto.
    pub fn create_with_override(
        &self,
        device: std::sync::Arc<GpuDevice>,
        gen_type: &manifold_core::PresetTypeId,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        width: u32,
        height: u32,
        is_watched: bool,
        manifest: Option<&manifold_core::params::ParamManifest>,
        relight: Option<&RelightParams>,
    ) -> Option<Box<PresetRuntime>> {
        let registry = PrimitiveRegistry::with_builtin();

        // The "effective def" this layer renders: the per-layer graph override
        // when present, else the bundled canonical preset parsed to a def. The
        // override wins over the bundled JSON; if it lost its `preset_metadata`
        // during a prior graph edit, graft it back from the bundle before
        // constructing — otherwise the bindings list would deserialize empty and
        // the live frame would render with every inner-node param pinned at its
        // JSON default *while the editor canvas still shows correct routings*. That
        // mismatch is silent and load-bearing on every graph-edit command's
        // `preset_metadata` preservation; the graft here is the durable defense.
        let (effective_def, is_override) = if let Some(def) = override_def {
            let mut grafted = def.clone();
            graft_preset_metadata_from_bundle(&mut grafted, gen_type);
            (Some(grafted), true)
        } else {
            let parsed = bundled_preset_json(gen_type).and_then(|json| {
                serde_json::from_str::<manifold_core::effect_graph_def::EffectGraphDef>(&json).ok()
            });
            (parsed, false)
        };

        if let Some(def) = effective_def {
            // D8/P7: relight now fuses. Augment with DEFAULT knob values before
            // the fusion compiler so the fused-generator cache key (and generated
            // WGSL) is knob-invariant; the live values are written per-frame via
            // `PresetRuntime::set_relight_params`.
            let def_for_fusion = if relight.is_some() {
                crate::node_graph::relight::relight_augment(
                    &def,
                    &registry,
                    &RelightParams::default(),
                )
            } else {
                def
            };
            // On-demand fusion (design step 2): fuse THIS exact shape — shipped,
            // edited, or created — unless the editor is watching it (then unfused
            // so per-node preview can sample inner-node textures and edits render
            // live). `fused_generator_def_for` compiles-on-miss + caches by the
            // def's content, so an edited generator fuses on editor-close exactly
            // like a shipped one. The fused def loads through the SAME `from_def`
            // path — only the def changed (fused kernels + bindings retargeted onto
            // them) — so modulation keeps flowing. Same decision as the effect
            // rule, via the one shared `should_render_fused`.
            let render_def = if crate::node_graph::freeze::install::should_render_fused(is_watched)
            {
                match crate::node_graph::freeze::install::fused_generator_def_for(&def_for_fusion)
                {
                    Some(fused) => (*fused).clone(),
                    None => def_for_fusion,
                }
            } else {
                def_for_fusion
            };
            match PresetRuntime::from_def_with_device(
                render_def,
                &registry,
                std::sync::Arc::clone(&device),
                width,
                height,
                self.target_format,
                manifest,
            ) {
                Ok(g) => return Some(Box::new(g)),
                Err(e) => {
                    log::warn!(
                        "Generator {} failed to load from def: {e}",
                        gen_type.as_str(),
                    );
                }
            }

            // Runtime robustness: a broken per-layer override falls back to the
            // bundled canonical so the layer keeps rendering rather than going
            // black mid-show. (Not a migration stop-gap — a broken user graph is a
            // real, transient editing state.) The canonical itself is the
            // effective def in the non-override case, so this only runs for
            // overrides.
            if is_override && let Some(json) = bundled_preset_json(gen_type) {
                match PresetRuntime::from_json_str_with_device(
                    &json,
                    &registry,
                    device,
                    width,
                    height,
                    self.target_format,
                    manifest,
                ) {
                    Ok(g) => return Some(Box::new(g)),
                    Err(e) => {
                        log::warn!(
                            "Bundled fallback for generator {} also failed: {e}",
                            gen_type.as_str(),
                        );
                    }
                }
            }
        }

        log::warn!("Generator type {:?} not found in the preset catalog", gen_type);
        None
    }

    /// Every `PresetTypeId` known to this registry (the bundled JSON
    /// generator presets). Used by the picker UI to populate the
    /// "Add Generator" menu.
    pub fn known_type_ids(&self) -> Vec<manifold_core::PresetTypeId> {
        let mut out: Vec<manifold_core::PresetTypeId> =
            bundled_preset_type_ids(PresetKind::Generator).collect();
        out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        out
    }
}

/// BUG-146: compile every registered primitive's standalone codegen pipeline
/// into `device`'s shared compute-pipeline cache, unconditionally, ahead of
/// any real `run()` call. Structural fix, not atom-by-atom — walks
/// `PrimitiveRegistry::with_builtin()`'s full `known_type_ids()` list (the
/// same enumeration `freeze/classify.rs`'s meta-tests use to iterate every
/// shipping atom), constructs a fresh instance of each, and asks
/// `crate::node_graph::freeze::codegen::standalone_for_node` for its
/// standalone kernel text.
///
/// Atoms with no `wgsl_body` (hand-written pipelines — `render_scene`,
/// `gltf_texture_source`, `draw_*`, `wgsl_compute`'s user-authored kernels)
/// return `CodegenError::NoBody` and are silently skipped — nothing to
/// prewarm generically for those; `render_scene`/`gltf_texture_source` have
/// their own explicit prewarm calls above, `wgsl_compute`'s kernel is live
/// user content compiled on demand, and `draw_*` is BUG-114 (tracked
/// separately, not a de-facto exemption from anything this function claims).
///
/// A handful of atoms (`scale_offset_texture`, `gradient_central_diff`,
/// `rotate_vec2_by_angle`) support a non-default output format via
/// `standalone_for_spec_fmt` / `set_output_format`; a freshly-constructed
/// instance's `output_format()` is `None` (the default), so this sweep
/// compiles their DEFAULT-format kernel — the common case. Only a project
/// whose JSON sets a non-default `outputFormats` on one of those three still
/// pays a lazy first-use compile for that specific variant; documented as a
/// residual, not silently dropped.
///
/// One real exemption, found empirically rather than by design: an atom that
/// declares `wgsl_specialization` (today, exactly one — `node.variable_blur`
/// / `gaussian_blur_variable_width.rs`) has FREE IDENTIFIERS in its generated
/// standalone text (`QUALITY_LEVEL`, `WEIGHTING_MODE`) that only resolve once
/// its own `run()` substitutes live param values via
/// `device.create_specialized_compute_pipeline` — plain `standalone_for_node`
/// followed by `create_compute_pipeline` fails a real naga parse on it
/// (confirmed: `WGSL parse error: no definition in scope for identifier:
/// WEIGHTING_MODE`).
/// The substitution VALUES (and their string encoding — `"0u"`/`"1u"`/`"2u"`
/// for quality, etc.) are genuinely bespoke per atom, not derivable from
/// `PrimitiveSpec`'s const data the way everything else in this sweep is —
/// there is no generic "default param value → specialization token string"
/// mapping to fall back on. Detected dynamically via
/// `EffectNode::wgsl_specialization()` (non-empty) and skipped rather than
/// crashing the sweep; `node.variable_blur`'s own first-use compile (up to 6
/// variants, `quality` × `weighting_mode`) stays a lazy first-frame cost,
/// same as before this fix. If a future atom adopts `wgsl_specialization`,
/// it lands in this same skip bucket automatically — no per-atom list to
/// maintain, just a residual class this sweep can't reach generically.
fn prewarm_all_atom_codegen_pipelines(device: &std::sync::Arc<GpuDevice>) {
    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_node};

    let registry = PrimitiveRegistry::with_builtin();
    let mut warmed = 0usize;
    let mut skipped_no_body = 0usize;
    let mut skipped_specialized = 0usize;
    let mut codegen_failed = 0usize;
    for type_id in registry.known_type_ids() {
        let Some(node) = registry.construct(type_id) else {
            continue;
        };
        if !node.wgsl_specialization().is_empty() {
            skipped_specialized += 1;
            continue;
        }
        match standalone_for_node(node.as_ref()) {
            Ok(wgsl) => {
                device.create_compute_pipeline(&wgsl, ENTRY, type_id);
                warmed += 1;
            }
            Err(crate::node_graph::freeze::codegen::CodegenError::NoBody) => {
                skipped_no_body += 1;
            }
            Err(e) => {
                // Should not happen for any shipping atom (every `wgsl_body`
                // atom's standalone codegen is proven at conversion time via
                // the I1 parity test) — log and move on rather than let a
                // prewarm-time codegen edge case take down startup.
                log::warn!("Pre-warm codegen failed for atom {type_id}: {e:?}");
                codegen_failed += 1;
            }
        }
    }
    log::info!(
        "Pre-warmed {warmed} atom codegen pipelines ({skipped_no_body} no-body, \
         {skipped_specialized} specialized-token atoms skipped, {codegen_failed} codegen errors)"
    );
}

/// If `def.preset_metadata` is `None`, parse the bundled JSON for
/// `gen_type` and graft its `preset_metadata` onto `def` in-place.
/// No-op if the override already carries metadata, or if no bundled
/// preset matches the type id (legacy Rust-only generator).
///
/// This is the single durable defense against the "edit command
/// dropped the `preset_metadata` and bindings vanished" failure mode
/// — without it, the runtime would render with every inner-node
/// param pinned at its JSON default while the editor canvas still
/// shows correct routings (silent mismatch). The
/// `content_thread::graph_snapshot` path mirrors this graft on the
/// snapshot side (generator branch) so both surfaces resolve to the
/// same set of bindings.
pub fn graft_preset_metadata_from_bundle(
    def: &mut manifold_core::effect_graph_def::EffectGraphDef,
    gen_type: &manifold_core::PresetTypeId,
) {
    if def.preset_metadata.is_some() {
        return;
    }
    let Some(json) = bundled_preset_json(gen_type) else {
        return;
    };
    let Ok(base) = serde_json::from_str::<manifold_core::effect_graph_def::EffectGraphDef>(&json)
    else {
        return;
    };
    def.preset_metadata = base.preset_metadata;
}

/// BUG-146 — GPU-backed proof that [`prewarm_all_atom_codegen_pipelines`]
/// actually populates the device's shared compute-pipeline cache for atoms on
/// the codegen path, so their first live `run()`'s
/// `self.pipeline.get_or_insert_with(...)` hits a cache entry instead of
/// compiling on the content thread. Scoped to one representative atom from
/// each of this session's three conversion waves — `node.grid_mesh` (wave
/// 2/mesh atoms), `node.shininess` (wave 2/lighting), `node.rotate_coordinates`
/// (wave 3) — rather than the full ~135-atom sweep, following the same
/// before/after + idempotent shape as
/// `render_scene::gpu_tests::prewarm_pipelines_populates_the_shared_render_cache`
/// and `gltf_texture_source::gpu_tests::prewarm_pipeline_populates_the_shared_compute_cache`.
/// Run deliberately: `cargo test -p manifold-renderer --features gpu-proofs
/// generators::registry::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_node};

    #[test]
    fn prewarm_populates_the_shared_cache_for_representative_converted_atoms() {
        let device = crate::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let sample = ["node.grid_mesh", "node.shininess", "node.rotate_coordinates"];

        let before = device.compute_pipeline_cache_len();
        prewarm_all_atom_codegen_pipelines(&device.arc());
        let after = device.compute_pipeline_cache_len();
        assert!(
            after >= before,
            "prewarm_all_atom_codegen_pipelines must never shrink the cache: before={before} after={after}"
        );

        // Idempotent: a second sweep must be a pure cache hit, not add more
        // entries.
        prewarm_all_atom_codegen_pipelines(&device.arc());
        assert_eq!(
            device.compute_pipeline_cache_len(),
            after,
            "a second atom-codegen prewarm pass must be a pure cache hit, not add more entries"
        );

        // After prewarm, each sampled atom's EXACT standalone-kernel compile
        // call — what its real `run()`'s `get_or_insert_with` would make on
        // its first live frame — must be a cache hit, not a fresh compile.
        // Order-independent by design (doesn't require the cache to be
        // empty beforehand): `device` is process-global across the whole
        // `--features gpu-proofs --lib` run (`crate::test_device()`), so
        // another test's `GeneratorRenderer::new` (which itself calls
        // `GeneratorRegistry::prewarm_all`) may have warmed these same three
        // atoms before this test runs — the same cross-test-ordering class
        // BUG-144 documents for the sibling render_scene/gltf_texture_source
        // prewarm tests. Asserting "warm after MY prewarm call" rather than
        // "count grew by exactly N" is correct either way: if this test's
        // own sweep is what warmed them, or another test's did, the
        // operationally meaningful fact — first live use is a cache hit —
        // holds either way.
        for type_id in sample {
            let node = registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("{type_id} must be registered"));
            let wgsl = standalone_for_node(node.as_ref())
                .unwrap_or_else(|e| panic!("{type_id} standalone codegen: {e:?}"));
            let cache_before_use = device.compute_pipeline_cache_len();
            device.create_compute_pipeline(&wgsl, ENTRY, type_id);
            assert_eq!(
                device.compute_pipeline_cache_len(),
                cache_before_use,
                "{type_id}'s standalone pipeline compile after prewarm must be a cache hit"
            );
        }
    }
}
