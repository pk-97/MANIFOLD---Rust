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
                    json,
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

    /// Create a new generator instance for the given type. JSON
    /// presets are consulted first; falls back to Rust factories.
    pub fn create(
        &self,
        device: &GpuDevice,
        gen_type: &manifold_core::GeneratorTypeId,
    ) -> Option<Box<dyn Generator>> {
        self.create_with_override(device, gen_type, None)
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
    ) -> Option<Box<dyn Generator>> {
        let registry = PrimitiveRegistry::with_builtin();

        // Override path: the layer's per-instance graph wins over the
        // bundled JSON when present.
        if let Some(def) = override_def {
            match JsonGraphGenerator::from_def_with_device(
                def.clone(),
                &registry,
                device,
                1920,
                1080,
                self.target_format,
            ) {
                Ok(g) => return Some(Box::new(g) as Box<dyn Generator>),
                Err(e) => {
                    log::warn!(
                        "Per-layer override for generator {} failed to load: {e} — \
                         falling back to bundled preset",
                        gen_type.as_str(),
                    );
                }
            }
        }

        // Bundled JSON preset path.
        if let Some(json) = bundled_generator_preset_json(gen_type) {
            match JsonGraphGenerator::from_json_str_with_device(
                json,
                &registry,
                device,
                // Initial size — resize() comes later. Pick a sane
                // non-tiny default so first-frame allocations don't
                // hit zero-sized texture warnings.
                1920,
                1080,
                self.target_format,
            ) {
                Ok(g) => return Some(Box::new(g) as Box<dyn Generator>),
                Err(e) => {
                    log::warn!(
                        "Failed to construct JSON generator {}: {e}",
                        gen_type.as_str(),
                    );
                    // Fall through to Rust factories — maybe a Rust
                    // factory by the same id is also registered.
                }
            }
        }

        // Rust factory fallback.
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
