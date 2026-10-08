use super::*;

pub const THUMB_W: u32 = super::THUMB_W;
pub const THUMB_H: u32 = super::THUMB_H;

pub trait GeneratorRendererTestkit {
    #[allow(clippy::too_many_arguments)]
    fn acquire_clip(
        &mut self,
        clip_id: &str,
        gen_type: PresetTypeId,
        layer_id: LayerId,
        layer_index: i32,
        clip_index: u32,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: u32,
        param_version: u32,
        clip_edge_enabled: bool,
        modifier_event: Option<(&manifold_core::effects::PresetInstance, bool)>,
        // The layer's live per-instance manifest, forwarded to
        // `install_layer_generator` when this clip start triggers a build so
        // the reshape sources from the manifest, not the stale shadow (BUG-078).
        manifest: Option<&ParamManifest>,
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5) — the layer's live
        // toggle + knobs.
        relight: bool,
        relight_params: manifold_core::effects::RelightParams,
    ) -> bool;
    #[allow(clippy::too_many_arguments)]
    fn install_layer_generator(
        &mut self,
        layer_id: LayerId,
        gen_type: PresetTypeId,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: Option<u32>,
        param_version: Option<u32>,
        clip_count: u32,
        audio_count: u32,
        layer_string_defaults: std::collections::BTreeMap<String, String>,
        // The layer's live per-instance param manifest (`gen_params.params`),
        // threaded into the generator build so a post-calibration rebuild
        // sources each param's reshape range/curve/invert from the manifest
        // authority, not the graph's stale `preset_metadata.params` shadow
        // (BUG-078). `None` for the type-swap path (fresh bundled build).
        manifest: Option<&ParamManifest>,
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5).
        relight: bool,
        relight_params: manifold_core::effects::RelightParams,
    ) -> bool;
}

impl GeneratorRendererTestkit for GeneratorRenderer {
    #[allow(clippy::too_many_arguments)]
    fn acquire_clip(
        &mut self,
        clip_id: &str,
        gen_type: PresetTypeId,
        layer_id: LayerId,
        layer_index: i32,
        clip_index: u32,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: u32,
        param_version: u32,
        clip_edge_enabled: bool,
        modifier_event: Option<(&manifold_core::effects::PresetInstance, bool)>,
        // The layer's live per-instance manifest, forwarded to
        // `install_layer_generator` when this clip start triggers a build so
        // the reshape sources from the manifest, not the stale shadow (BUG-078).
        manifest: Option<&ParamManifest>,
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5) — the layer's live
        // toggle + knobs.
        relight: bool,
        relight_params: manifold_core::effects::RelightParams,
    ) -> bool {
        GeneratorRenderer::acquire_clip(self, clip_id, gen_type, layer_id, layer_index, clip_index, override_def, override_version, param_version, clip_edge_enabled, modifier_event, manifest, relight, relight_params)
    }
    #[allow(clippy::too_many_arguments)]
    fn install_layer_generator(
        &mut self,
        layer_id: LayerId,
        gen_type: PresetTypeId,
        override_def: Option<&manifold_core::effect_graph_def::EffectGraphDef>,
        override_version: Option<u32>,
        param_version: Option<u32>,
        clip_count: u32,
        audio_count: u32,
        layer_string_defaults: std::collections::BTreeMap<String, String>,
        // The layer's live per-instance param manifest (`gen_params.params`),
        // threaded into the generator build so a post-calibration rebuild
        // sources each param's reshape range/curve/invert from the manifest
        // authority, not the graph's stale `preset_metadata.params` shadow
        // (BUG-078). `None` for the type-swap path (fresh bundled build).
        manifest: Option<&ParamManifest>,
        // "3D Shading" (`docs/DEPTH_RELIGHT_DESIGN.md` P5).
        relight: bool,
        relight_params: manifold_core::effects::RelightParams,
    ) -> bool {
        GeneratorRenderer::install_layer_generator(self, layer_id, gen_type, override_def, override_version, param_version, clip_count, audio_count, layer_string_defaults, manifest, relight, relight_params)
    }
}
