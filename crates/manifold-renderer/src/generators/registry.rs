use crate::generator::Generator;
use crate::generators::bundled_generator_presets::{
    bundled_generator_preset_json, bundled_generator_preset_type_ids,
};
use crate::generators::json_graph_generator::JsonGraphGenerator;
use crate::node_graph::PrimitiveRegistry;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

/// Factory that maps GeneratorTypeId to concrete Generator instances.
/// Pipeline compilation happens at creation time (expensive — do at startup or first use).
///
/// Two registration sources, consulted in this order:
/// 1. **Bundled JSON presets** at `assets/generator-presets/*.json`,
///    embedded by `build.rs`. Each becomes a [`JsonGraphGenerator`]
///    instance.
/// 2. **Rust factories** registered via `inventory::submit!` in each
///    generator's implementation file (the legacy path; gradually being
///    replaced by JSON presets as Tier 1 / Tier 2 / Tier 3 migrations
///    land).
///
/// JSON takes priority — if a `<TypeId>.json` ships in
/// `assets/generator-presets/`, the registry uses that even if a Rust
/// factory for the same id is also present (so a JSON preset can
/// supersede a legacy Rust implementation without removing the Rust
/// code first).
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
    pub fn prewarm_all(&self, device: &GpuDevice) {
        let rust_factories: Vec<_> = inventory::iter::<super::registration::GeneratorFactory>
            .into_iter()
            .collect();
        let json_count = bundled_generator_preset_type_ids().count();
        log::info!(
            "Pre-warming {} Rust + {} JSON generator pipelines...",
            rust_factories.len(),
            json_count,
        );
        for factory in &rust_factories {
            let _ = (factory.create)(device);
        }
        // Pre-warm JSON-defined generators too. We need a default
        // render resolution here — use a small placeholder; real sizes
        // come through on the first frame's `resize`. The pipelines
        // baked into each primitive cache at first dispatch regardless.
        let registry = PrimitiveRegistry::with_builtin();
        for type_id in bundled_generator_preset_type_ids() {
            if let Some(json) = bundled_generator_preset_json(&type_id)
                && let Err(e) = JsonGraphGenerator::from_json_str_with_device(
                    &json,
                    &registry,
                    device,
                    256,
                    256,
                    self.target_format,
                )
            {
                log::warn!(
                    "Pre-warm of bundled generator preset {} failed: {e}",
                    type_id.as_str(),
                );
            }
        }
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
        device: &GpuDevice,
        gen_type: &manifold_core::GeneratorTypeId,
        width: u32,
        height: u32,
    ) -> Option<Box<dyn Generator>> {
        // No override, no watch context (perf-gate tuning / tests / non-editor
        // call sites) — fuse normally per the device verdict.
        self.create_with_override(device, gen_type, None, width, height, false)
    }

    /// Same as [`Self::create`] but routes a per-layer
    /// `EffectGraphDef` override (from `Layer::generator_graph`)
    /// straight into [`JsonGraphGenerator::from_def_with_device`].
    /// `override_def = None` falls back to the bundled JSON preset.
    ///
    /// Returns `None` if neither the override nor the bundled preset
    /// can be loaded AND no Rust factory matches.
    ///
    /// **Important**: the override path is JSON-graph-only. Rust
    /// generators (the legacy `inventory::submit!` factories) can't
    /// be overridden — the override field on the layer is silently
    /// ignored in that case, the Rust factory's `create` runs as
    /// usual. Rust-only generators don't surface in the graph editor,
    /// so the layer's override field can't have been populated
    /// against them via the normal UI flow.
    pub fn create_with_override(
        &self,
        device: &GpuDevice,
        gen_type: &manifold_core::GeneratorTypeId,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        width: u32,
        height: u32,
        is_watched: bool,
    ) -> Option<Box<dyn Generator>> {
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
            let parsed = bundled_generator_preset_json(gen_type).and_then(|json| {
                serde_json::from_str::<manifold_core::effect_graph_def::EffectGraphDef>(&json).ok()
            });
            (parsed, false)
        };

        if let Some(def) = effective_def {
            // On-demand fusion (design step 2): fuse THIS exact shape — shipped,
            // edited, or created — unless the editor is watching it (then unfused
            // so per-node preview can sample inner-node textures and edits render
            // live). `fused_generator_def_for` compiles-on-miss + caches by the
            // def's content, so an edited generator fuses on editor-close exactly
            // like a shipped one. The fused def loads through the SAME `from_def`
            // path — only the def changed (fused kernels + bindings retargeted onto
            // them) — so modulation keeps flowing. Same decision as the effect
            // rule, via the one shared `should_render_fused`.
            let render_def = if crate::node_graph::freeze::install::should_render_fused(
                crate::node_graph::freeze::install::FuseTarget::Generator(gen_type),
                is_watched,
            ) {
                match crate::node_graph::freeze::install::fused_generator_def_for(&def) {
                    Some(fused) => fused.clone(),
                    None => def,
                }
            } else {
                def
            };
            match JsonGraphGenerator::from_def_with_device(
                render_def,
                &registry,
                device,
                width,
                height,
                self.target_format,
            ) {
                Ok(g) => return Some(Box::new(g) as Box<dyn Generator>),
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
            if is_override && let Some(json) = bundled_generator_preset_json(gen_type) {
                match JsonGraphGenerator::from_json_str_with_device(
                    &json,
                    &registry,
                    device,
                    width,
                    height,
                    self.target_format,
                ) {
                    Ok(g) => return Some(Box::new(g) as Box<dyn Generator>),
                    Err(e) => {
                        log::warn!(
                            "Bundled fallback for generator {} also failed: {e}",
                            gen_type.as_str(),
                        );
                    }
                }
            }
        }

        // Rust factory fallback. Rust generators allocate their
        // internal resources lazily on first `resize()` (called by
        // `GeneratorRenderer::resize_gpu`), so the canvas dims aren't
        // needed at construction here — they only matter for the
        // JSON chain-build's canvas-sized array pre-allocation
        // handled above.
        for factory in inventory::iter::<super::registration::GeneratorFactory> {
            if factory.id == *gen_type {
                return Some((factory.create)(device));
            }
        }
        log::warn!("Generator type {:?} not yet implemented", gen_type);
        None
    }

    /// Every `GeneratorTypeId` known to this registry — both JSON
    /// presets and Rust factories. Used by the picker UI to populate
    /// the "Add Generator" menu.
    pub fn known_type_ids(&self) -> Vec<manifold_core::GeneratorTypeId> {
        let mut out: Vec<manifold_core::GeneratorTypeId> =
            bundled_generator_preset_type_ids().collect();
        for factory in inventory::iter::<super::registration::GeneratorFactory> {
            // Avoid duplicating ids that ship in both sources.
            if !out.contains(&factory.id) {
                out.push(factory.id.clone());
            }
        }
        out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        out
    }
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
/// `content_thread::active_generator_graph_snapshot` path mirrors
/// this graft on the snapshot side so both surfaces resolve to the
/// same set of bindings.
pub fn graft_preset_metadata_from_bundle(
    def: &mut manifold_core::effect_graph_def::EffectGraphDef,
    gen_type: &manifold_core::GeneratorTypeId,
) {
    if def.preset_metadata.is_some() {
        return;
    }
    let Some(json) = bundled_generator_preset_json(gen_type) else {
        return;
    };
    let Ok(base) = serde_json::from_str::<manifold_core::effect_graph_def::EffectGraphDef>(&json)
    else {
        return;
    };
    def.preset_metadata = base.preset_metadata;
}
