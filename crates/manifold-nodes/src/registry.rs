use crate::bundled_presets::{bundled_preset_def, bundled_preset_json, bundled_preset_type_ids};
use {manifold_nodes_scene::node_graph::primitives::gltf_texture_source::GltfTextureSource, manifold_nodes_scene::node_graph::primitives::render_scene::RenderScene, manifold_nodes_scene::node_graph::primitives::scatter_on_mesh::ScatterOnMesh, manifold_nodes_image::node_graph::primitives::seed_particles_from_texture::SeedParticlesFromTexture};
use manifold_node_engine::runtime::PresetRuntime;
use manifold_core::effects::RelightParams;
use manifold_core::preset_def::PresetKind;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

mod provider;

/// Factory for catalog generator [`PresetRuntime`] instances.
///
/// Generator presets are loaded from the catalog, and graph-specific pipelines
/// are prepared by the installed nodes. Fixed and specialized pipelines are
/// warmed through the node hooks in [`Self::prewarm_all`].
pub struct GeneratorRegistry {
    target_format: GpuTextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: GpuTextureFormat) -> Self {
        Self { target_format }
    }

    /// Prewarm fixed generator and encoder pipelines. Graph-specific WGSL is
    /// prepared by node installation. Call before `save_pipeline_archive()`.
    pub fn prewarm_all(&self, device: &std::sync::Arc<GpuDevice>) {
        device.prepare_utility_pipelines();
        let json_count = bundled_preset_type_ids(PresetKind::Generator).count();
        log::info!("Pre-warming {json_count} JSON generator pipelines...");
        // Building every catalog runtime allocates all of its graph resources;
        // keep that validation opt-in. Installed graphs prepare their own
        // authored and fused WGSL through node hooks.
        let preset_prewarm = std::env::var_os("MANIFOLD_PRESET_PREWARM").is_some();
        // Optionally validate catalog generator definitions at a small
        // placeholder resolution. Live sizes arrive through the first resize.
        let registry = PrimitiveRegistry::with_builtin();
        if preset_prewarm {
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
        }

        // These hand-written and specialized pipelines are outside the generic
        // atom sweep below. Warm them through their node hooks so first use of
        // a project does not pay their fixed pipeline compile cost.
        //
        // `node.render_scene` is a hand-written EffectNode, not `primitive!`,
        // with MSAA depth render-pipeline variants keyed on material kind /
        // blend / velocity+AO+denoise auxiliary outputs.
        RenderScene::prewarm_pipelines(device);
        manifold_nodes_scene::node_graph::primitives::render_mesh_diagram::RenderMeshDiagram::prewarm_pipelines(device);
        // The RT shadow-ray pipeline set is device-global; warm it before the
        // first RenderScene construction.
        manifold_gpu::raytrace::MetalShadowRayTracer::prewarm(device);
        // `node.gltf_texture_source` uses a hand-written runtime blit, so the
        // atom sweep skips it.
        GltfTextureSource::prewarm_pipeline(device);
        manifold_nodes_water::primitives::physics_world::PhysicsWorldNode::prewarm_pipeline(device);
        manifold_nodes_image::node_graph::primitives::terminal_analysis::prewarm_pipeline(device);
        // These multi-pass nodes use specialized hooks rather than the generic
        // standalone codegen path.
        ScatterOnMesh::prewarm_pipelines(device);
        SeedParticlesFromTexture::prewarm_pipelines(device);

        manifold_nodes_image::node_graph::primitives::watercolor::Watercolor::prewarm_pipelines(device);
        manifold_nodes_scene::node_graph::primitives::hdri_source::HdriSource::prewarm_pipeline(device);
        manifold_nodes_image::node_graph::primitives::layer_source::LayerSource::prewarm_pipeline(device);
        manifold_nodes_image::node_graph::primitives::gaussian_blur_variable_width::GaussianBlurVariableWidth::prewarm_pipelines(device);
        manifold_nodes_image::node_graph::primitives::multi_blend::MultiBlend::prewarm_pipelines(device);
        manifold_nodes_image::node_graph::primitives::filter::Blur::prewarm_pipelines(device);
        manifold_nodes_image::node_graph::primitives::bokeh_gather::BokehGather::prewarm_pipelines(device);

        // Sweep every registered generic atom so standalone codegen pipelines
        // are ready before live graph execution. Hand-written and
        // specialization-token nodes remain covered by their hooks above.
        prewarm_all_atom_codegen_pipelines(device);

        log::info!("Generator pipeline pre-warm complete");
    }

    /// Create a generator from the catalog at the host's current canvas
    /// resolution.
    ///
    /// `width`/`height` are the live canvas dimensions. Pass the real size so
    /// canvas-sized array outputs allocate correctly on first construction;
    /// this path intentionally has no resolution fallback.
    pub fn create(
        &self,
        device: std::sync::Arc<GpuDevice>,
        gen_type: &manifold_core::PresetTypeId,
        width: u32,
        height: u32,
    ) -> Option<Box<PresetRuntime>> {
        // No override or watch context: use the normal fusion policy and the
        // definition's own parameter metadata.
        self.create_with_override(device, gen_type, None, width, height, false, None, None)
    }

    /// Same as [`Self::create`] but routes a per-layer
    /// `EffectGraphDef` override (from `Layer::generator_graph`)
    /// straight into [`PresetRuntime::from_def_with_device`].
    /// `override_def = None` uses the catalog definition for `gen_type`.
    ///
    /// `manifest` is the live per-instance [`manifold_core::params::ParamManifest`] used during a
    /// project-generator rebuild; it supplies range, curve, and invert values
    /// when present. Non-instance callers use the definition metadata.
    ///
    /// Returns `None` if neither the override nor the catalog definition loads.
    ///
    /// `relight` is the "3D Shading" toggle at the compile level:
    /// `Some(params)` augments the effective def with default relight knobs
    /// before `from_def_for_render`; live knobs are applied afterward by
    /// `PresetRuntime::set_relight_params`. `None` leaves the def unchanged.
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

        // The override wins when present. If an edit dropped its metadata,
        // restore the catalog metadata before constructing the runtime so the
        // editor and renderer keep the same binding authority.
        let (effective_def, is_override) = if let Some(def) = override_def {
            let mut grafted = def.clone();
            graft_preset_metadata_from_bundle(&mut grafted, gen_type);
            (Some(grafted), true)
        } else {
            // Use the migrated catalog definition so generated scene bindings
            // are present in the runtime view.
            let parsed = bundled_preset_def(gen_type).map(|def| (*def).clone());
            (parsed, false)
        };

        if let Some(def) = effective_def {
            // An authored CPU-FLIP graph is project content. If it cannot
            // instantiate, do not replace it with the catalog canonical.
            let preserve_authored_graph = override_def.is_some_and(|def| {
                manifold_core::retired_cpu_flip::preserve_authored_cpu_flip_graph(
                    gen_type, def,
                )
            });

            // Augment with default relight values before fusion so the fused
            // cache key and generated WGSL are knob-invariant. Live values are
            // written afterward through `PresetRuntime::set_relight_params`.
            let def_for_fusion = if relight.is_some() {
                manifold_nodes_scene::node_graph::relight::relight_augment(
                    &def,
                    &registry,
                    &RelightParams::default(),
                )
            } else {
                def
            };
            // Watched graphs stay unfused for live editing; otherwise the
            // shared render policy may use the fused view for this exact def.
            // Relight augmentation above does not change that policy.
            match PresetRuntime::from_def_for_render(
                def_for_fusion,
                &registry,
                manifest,
                manifold_node_engine::freeze::install::should_render_fused(is_watched),
            ).and_then(|runtime| runtime.with_generator_device(
                std::sync::Arc::clone(&device), width, height, self.target_format,
            )) {
                Ok(g) => return Some(Box::new(g)),
                Err(e) => {
                    log::warn!(
                        "Generator {} failed to load from def: {e}",
                        gen_type.as_str(),
                    );
                }
            }

            // A broken per-layer override may fall back to the catalog canonical
            // so a transient editing state keeps rendering. Authored CPU-FLIP
            // graphs and modifier stacks must surface failure instead of losing
            // their project content through this fallback.
            if is_override
                && !preserve_authored_graph
                && override_def.is_none_or(|def| def.scene_modifiers.is_empty())
                && let Some(def) = bundled_preset_def(gen_type) {
                match PresetRuntime::from_def_with_device(
                    (*def).clone(),
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
}

/// Prepare each registered primitive through its node hook, then compile its
/// generic standalone pipeline when available. Hand-written nodes have no
/// generic body; specialized nodes supply their variants through the hook.
fn prewarm_all_atom_codegen_pipelines(device: &std::sync::Arc<GpuDevice>) {
    use manifold_node_engine::freeze::codegen::{ENTRY, standalone_for_node};

    let registry = PrimitiveRegistry::with_builtin();
    let mut warmed = 0usize;
    let mut skipped_no_body = 0usize;
    let mut skipped_specialized = 0usize;
    let mut codegen_failed = 0usize;
    for type_id in registry.known_type_ids() {
        let Some(mut node) = registry.construct(type_id) else {
            continue;
        };
        node.prepare_pipelines(device);
        if !node.wgsl_specialization().is_empty() {
            skipped_specialized += 1;
            continue;
        }
        match standalone_for_node(node.as_ref()) {
            Ok(wgsl) => {
                device.create_compute_pipeline(&wgsl, ENTRY, type_id);
                warmed += 1;
            }
            Err(manifold_node_engine::freeze::codegen::CodegenError::NoBody) => {
                skipped_no_body += 1;
            }
            Err(e) => {
                // Keep startup alive if a generic codegen edge case appears;
                // the affected atom will report the same failure on use.
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

/// If `def.preset_metadata` is `None`, graft the catalog canonical's migrated
/// metadata onto `def`. This preserves the live binding authority after an
/// edit command drops metadata; overrides that already carry metadata are
/// unchanged.
pub fn graft_preset_metadata_from_bundle(
    def: &mut manifold_core::effect_graph_def::EffectGraphDef,
    gen_type: &manifold_core::PresetTypeId,
) {
    if def.preset_metadata.is_some() {
        return;
    }
    let Some(bundled) = bundled_preset_def(gen_type) else {
        return;
    };
    def.preset_metadata = bundled.preset_metadata.clone();
}

/// GPU-backed proof that [`prewarm_all_atom_codegen_pipelines`] populates the
/// shared cache and is idempotent for representative generic atoms.
/// Run deliberately: `cargo test -p manifold-nodes --features gpu-proofs
/// registry::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::freeze::codegen::{ENTRY, standalone_for_node};

    #[test]
    fn prewarm_populates_the_shared_cache_for_representative_converted_atoms() {
        let device = manifold_gpu::testkit::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let sample = ["node.grid_mesh", "node.shininess", "node.rotate_coordinates"];

        let before = device.compute_pipeline_cache_len();
        prewarm_all_atom_codegen_pipelines(&device.arc());
        let after = device.compute_pipeline_cache_len();
        assert!(
            after >= before,
            "prewarm_all_atom_codegen_pipelines must never shrink the cache: before={before} after={after}"
        );

        // A second sweep must be a pure cache hit.
        prewarm_all_atom_codegen_pipelines(&device.arc());
        assert_eq!(
            device.compute_pipeline_cache_len(),
            after,
            "a second atom-codegen prewarm pass must be a pure cache hit, not add more entries"
        );

        // Each sampled atom's standalone compile must now be a cache hit.
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
