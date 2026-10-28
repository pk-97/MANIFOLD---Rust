//! Construction: RenderScene::new, and rebuild, which sizes the input ports and params for
//! a given object and light count and resets the per-object histories.

use super::*;

impl RenderScene {
    pub fn new() -> Self {
        let mut s = Self {
            inputs: Vec::new(),
            params: Vec::new(),
            num_objects: 0,
            num_lights: 0,
            pipelines: AHashMap::new(),
            depth_stencil: None,
            blend_depth_stencil: None,
            transmission_depth_stencil: None,
            msaa_color: None,
            depth_texture: None,
            msaa_real: false,
            depth_resolve_scratch: None,
            depth_width: 0,
            depth_height: 0,
            velocity_msaa: None,
            velocity_width: 0,
            velocity_height: 0,
            ao_mask_msaa: None,
            ao_mask_width: 0,
            ao_mask_height: 0,
            denoise_normals_msaa: None,
            denoise_normals_width: 0,
            denoise_normals_height: 0,
            denoise_roughness_msaa: None,
            denoise_roughness_width: 0,
            denoise_roughness_height: 0,
            denoise_diffuse_albedo_msaa: None,
            denoise_diffuse_albedo_width: 0,
            denoise_diffuse_albedo_height: 0,
            denoise_specular_albedo_msaa: None,
            denoise_specular_albedo_width: 0,
            denoise_specular_albedo_height: 0,
            denoise_reactive_msaa: None,
            denoise_reactive_width: 0,
            denoise_reactive_height: 0,
            hit_dist_extract_pipeline: None,
            upscale_alpha_combine_pipeline: None,
            rt_temporal_color_scratch: None,
            rt_temporal_color_scratch_width: 0,
            rt_temporal_color_scratch_height: 0,
            rt_firefly_scratch: None,
            rt_firefly_params_buffer: None,
            rt_irr_filtered: None,
            rt_irr_filtered_b: None,
            rt_atrous_post_params_buffer: None,
            denoiser: None,
            denoiser_unavailable_logged: false,
            denoise_gate_blocked_logged: false,
            rt_temporal_upscaler: None,
            rt_temporal_unavailable_logged: false,
            prev_model: Vec::new(),
            prev_positions: Vec::new(),
            prev_instances: Vec::new(),
            prev_positions_dummy: None,
            pack_prev_positions_pipeline: None,
            prev_view_proj: None,
            prev_cam_state: None,
            rendered_camera: None,
            rendered_lights: Vec::new(),
            selection: Default::default(),
            pick: Default::default(),
            gizmo_depth_wanted: false,
            gizmo_depth_ready: false,
            prev_jitter_ndc: None,
            jitter_frame_index: 0,
            dummy_texture: None,
            dummy_emissive_buffer: None,
            sampler: None,
            refraction_sampler: None,
            material_samplers: AHashMap::default(),
            light_buffers: Vec::new(),
            light_frame: 0,
            light_capacity: 0,
            shadow_pipeline: None,
            solid_depth_pipeline: None,
            shadow_depth_stencil: None,
            shadow_maps: None,
            shadow_maps_refused: None,
            shadow_cache_keys: Vec::new(),
            shadow_sampler: None,
            dummy_depth: None,
            dummy_depth_array: None,
            dummy_mask_array: None,
            caster_offset: 0,
            identity_instance_stub: None,
            shaft_inscatter: None,
            shaft_depth_half: None,
            shaft_half_width: 0,
            shaft_half_height: 0,
            shaft_depth_internal: None,
            shaft_depth_internal_width: 0,
            shaft_depth_internal_height: 0,
            shaft_downsample_pipeline: None,
            shaft_march_pipeline: None,
            shaft_composite_pipeline: None,
            opaque_scene_color: None,
            opaque_scene_color_width: 0,
            opaque_scene_color_height: 0,
            opaque_scene_color_format: manifold_gpu::GpuTextureFormat::Rgba16Float,
            volume_optics: volume_optics::VolumeOptics::default(),
            wet_pass: Default::default(),
            foam_pass: Default::default(),
            caustics: Default::default(),
            water_march: Default::default(),
            water_shade: Default::default(),
            ripple_detail: Default::default(),
            live_draw_args: live_draw_args::LiveDrawArgs::default(),
            culled_instances: Vec::new(),
            culled_prev: Vec::new(),
            opaque_depth_snapshot: None,
            opaque_depth_snapshot_width: 0,
            opaque_depth_snapshot_height: 0,
            transmissive_depth_scratch: None,
            transmissive_depth_scratch_width: 0,
            transmissive_depth_scratch_height: 0,
            transmissive_depth_error: None,
            rt_tracer: None,
            rt_accel: None,
            #[cfg(feature = "gpu-proofs")]
            rt_probe: None,
            rt_accel_key: None,
            rt_accel_topo_key: None,
            rt_accel_content_key: None,
            rt_appearance_key: None,
            rt_mesh_revisions: Vec::new(),
            rt_changes: Vec::new(),
            rt_source_trace_last_admission: None,
            rt_mask_half: None,
            rt_mask_full: None,
            rt_sv_refused: None,
            rt_sv_needs_reset: false,
            rt_mask_width: 0,
            rt_mask_trace_w: 0,
            rt_mask_trace_h: 0,
            rt_mask_height: 0,
            rt_params_buffer: None,
            rt_gi_materials: None,
            rt_gi_materials_capacity: 0,
            rt_gi_upload: Vec::new(),
            rt_objects_scratch: Vec::new(),
            rt_materials_scratch: Vec::new(),
            rt_has_translucency: false,
            rt_prev_toggle_shadows: None,
            rt_prev_toggle_ao: None,
            rt_prev_toggle_gi: None,
            rt_prev_toggle_refl: None,
            rt_prev_accumulating: false,
            prev_temporal_upscale: false,
            rt_obj_motion: None,
            rt_obj_motion_capacity: 0,
            rt_motion_upload: Vec::new(),
            rt_normal_sources: None,
            rt_normal_sources_capacity: 0,
            rt_normal_sources_scratch: None,
            rt_normal_sources_scratch_capacity: 0,
            rt_irr_half: None,
            rt_irr_full: None,
            rt_refl_half: None,
            rt_refl_full: None,
            rt_refl_full_b: None,
            rt_svt_half: None,
            rt_svt_full: None,
            rt_svt_full_b: None,
            rt_svt_history: [None, None],
            rt_irr_history: [None, None],
            rt_refl_history: [None, None],
            rt_sv_history: [None, None],
            rt_sv_m1_history: [None, None],
            rt_sv_m2_history: [None, None],
            rt_sv_hold_history: [None, None],
            rt_depth_history: [None, None],
            rt_normal_history: [None, None],
            rt_moments_history: [None, None],
            rt_moments_valid: false,
            rt_history_ping: 0,
            rt_nonpbr_warned: false,
            rt_normal_half: None,
            rt_normal_full: None,
            rt_mask_full_b: None,
            rt_irr_full_b: None,
            rt_normal_full_b: None,
            rt_atrous_params_buffer: None,
            rt_lighting_key: None,
            rt_lighting_geo_key: None,
            rt_lighting_gesture: 0,
            rt_lighting_prev_changed: false,
            rt_lighting_geo_gesture: 0,
            rt_lighting_geo_prev_changed: false,
            rt_irr_width: 0,
            rt_irr_trace_w: 0,
            rt_irr_trace_h: 0,
            rt_irr_height: 0,
            rt_accumulate_params_buffer: None,
            rt_reset_detector: TemporalResetDetector::new(),
            mesh_topology_history: Default::default(),
            rt_irr_needs_reset: false,
            denoiser_lighting_changed: false,
            denoiser_gesture_active: false,
            prefiltered_specular: None,
            prefiltered_sheen: None,
            subsurface_pass: Default::default(),
            irradiance_map: None,
            brdf_lut: None,
            brdf_lut_built: false,
            ibl_cache_key: None,
            ibl_prefilter_pipeline: None,
            ibl_irradiance_pipeline: None,
            ibl_brdf_lut_pipeline: None,
            object_port_names: Vec::new(),
            light_port_names: Vec::new(),
        };

        s.rebuild(DEFAULT_OBJECTS, DEFAULT_LIGHTS);
        s
    }

    /// (Re)build the input port list AND the parameter list for
    /// `objects` objects / `lights` lights (each clamped to its cap).
    pub(super) fn rebuild(&mut self, objects: u32, lights: u32) {
        let objects = objects.clamp(1, OBJECT_SAFETY_MAX);
        let lights = lights.clamp(0, LIGHT_SLIDER_MAX);
        let n_obj = objects as usize;
        self.rt_objects_scratch
            .reserve(n_obj.saturating_sub(self.rt_objects_scratch.len()));
        self.rt_materials_scratch
            .reserve(n_obj.saturating_sub(self.rt_materials_scratch.len()));
        self.rt_mesh_revisions.resize(n_obj, None);
        self.rt_mesh_revisions.fill(None);
        self.rt_changes = Vec::with_capacity(n_obj);
        let n_lights = lights as usize;

        let mut inputs = Vec::with_capacity(3 + n_lights + n_obj * 9);
        inputs.push(NodePort {
            name: std::borrow::Cow::Borrowed("camera"),
            ty: PortType::Camera,
            kind: PortKind::Input,
            required: true,
        });
        inputs.push(NodePort {
            name: std::borrow::Cow::Borrowed("envmap"),
            ty: PortType::Texture2D,
            kind: PortKind::Input,
            required: false,
        });
        // Optional scene-wide atmosphere (P3). Unwired = fog off,
        // byte-identical to no atmosphere.
        inputs.push(NodePort {
            name: std::borrow::Cow::Borrowed("atmosphere"),
            ty: PortType::Atmosphere,
            kind: PortKind::Input,
            required: false,
        });
        // Optional scene-wide render mode (SCENE_RENDER_MODE_DESIGN.md D2).
        // Unwired = Rendered, byte-identical to no render_mode.
        inputs.push(NodePort {
            name: std::borrow::Cow::Borrowed("render_mode_in"),
            ty: PortType::RenderMode,
            kind: PortKind::Input,
            required: false,
        });
        for i in 0..n_lights {
            inputs.push(NodePort {
                name: std::borrow::Cow::Owned(format!("light_{i}")),
                ty: PortType::Light,
                kind: PortKind::Input,
                required: false,
            });
        }
        // One `object_{i}:
        // Object` port per object, replacing the 21 legacy per-object port
        // families (mesh_i/material_i/17 maps/transform_i/instances_i).
        // `node.scene_object` binds all of that into one `SceneObject`;
        // unwired = skip (no draw, no shadow — D4). Optional, not required:
        // an in-progress edit that hasn't wired every object slot yet must
        // not error the whole scene, it just skips that object.
        for i in 0..n_obj {
            inputs.push(NodePort {
                name: std::borrow::Cow::Owned(format!("object_{i}")),
                ty: PortType::Object,
                kind: PortKind::Input,
                required: false,
            });
        }

        let params = vec![
            ParamDef {
                name: std::borrow::Cow::Borrowed("objects"),
                label: "Objects",
                ty: ParamType::Int,
                default: ParamValue::Float(DEFAULT_OBJECTS as f32),
                range: Some((1.0, OBJECT_SAFETY_MAX as f32)),
                enum_values: &[],
            },
            ParamDef {
                name: std::borrow::Cow::Borrowed("lights"),
                label: "Lights",
                ty: ParamType::Int,
                default: ParamValue::Float(DEFAULT_LIGHTS as f32),
                range: Some((0.0, LIGHT_SLIDER_MAX as f32)),
                enum_values: &[],
            },
            // Turns the `envmap` about the up axis, counter-clockwise seen
            // from above, at every lookup (raster and ray traced), so a live
            // turn never re-bakes the map or its IBL convolution.
            ParamDef {
                name: std::borrow::Cow::Borrowed("env_rotation"),
                label: "Environment Rotation",
                ty: ParamType::Angle,
                default: ParamValue::Float(0.0),
                range: Some((-std::f32::consts::PI, std::f32::consts::PI)),
                enum_values: &[],
            },
            // Scene-wide shading mode (SCENE_RENDER_MODE_DESIGN.md): Rendered,
            // Solid, Wireframe, Points. A wired `render_mode_in` input overrides it.
            ParamDef {
                name: std::borrow::Cow::Borrowed("render_mode"),
                label: "Render Mode",
                ty: ParamType::Enum,
                default: ParamValue::Enum(0),
                range: Some((
                    0.0,
                    (manifold_node_engine::scene::render_mode::RENDER_MODE_LABELS.len() - 1) as f32,
                )),
                enum_values: manifold_node_engine::scene::render_mode::RENDER_MODE_LABELS,
            },
            // RAYTRACING_DESIGN.md D14 (W0): per-scene RT toggle. Default
            // off — an untouched scene is byte-identical to before this
            // param existed (`force_consumed_outputs` returns empty unless
            // this is explicitly `true`). P1 wires the real scene-level
            // toggle (EditingService + serialization) through to this same
            // param; W0 only builds the render-pass side of the contract.
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_enabled"),
                label: "Ray Tracing",
                ty: ParamType::Bool,
                default: ParamValue::Bool(false),
                range: None,
                enum_values: &[],
            },
            // RAYTRACING_DESIGN.md section 5.2 P4 / section 8.2 D22 (T2-B): per-scene
            // MetalFX Temporal quality-mode toggle. Default off — an
            // untouched scene is byte-identical to before this param existed
            // (no jitter applied to `view_proj`, `force_consumed_outputs`
            // unaffected unless this is explicitly `true`). `true` drives the
            // real reduced-res-render + upscale path (D22): Pass A/B/shafts
            // render into `rt_temporal_color_scratch` at
            // `upscale_quality`'s fraction of native res, then
            // `rt_temporal_upscaler` upscales that scratch color (+ the
            // render-res `depth`/`velocity` graph outputs) to native res as
            // this node's `color` output.
            ParamDef {
                name: std::borrow::Cow::Borrowed("temporal_upscale"),
                label: "Temporal Upscale",
                ty: ParamType::Bool,
                default: ParamValue::Bool(false),
                range: None,
                enum_values: &[],
            },
            // How far down `temporal_upscale` renders before MetalFX brings the
            // picture back to native res. Inert while `temporal_upscale` is
            // off. Changing it resizes the compiled `depth`/`velocity` outputs,
            // so it rebuilds the scene like the toggle does. Quality is the
            // default: the fixed scale scenes had before the choice existed.
            ParamDef {
                name: std::borrow::Cow::Borrowed("upscale_quality"),
                label: "Upscale Quality",
                ty: ParamType::Enum,
                default: ParamValue::Enum(DEFAULT_UPSCALE_QUALITY),
                range: Some((0.0, (UPSCALE_QUALITIES.len() - 1) as f32)),
                enum_values: &UPSCALE_QUALITY_LABELS,
            },
            // RAYTRACING_DESIGN.md section 9 RD9 (T4): per-scene reflection toggle.
            // Default ON (Q3). Inert when `rt_enabled` false — no reflection
            // rays are dispatched unless the RT pipeline is active.
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_reflections"),
                label: "Reflections",
                ty: ParamType::Bool,
                default: ParamValue::Bool(true),
                range: None,
                enum_values: &[],
            },
            // RT toggles (section 17.x wave): per-term card params so Peter can
            // flip RT shadows / AO / GI on and off per scene from the layer
            // inspector. All default ON = today's behavior. Inert when
            // `rt_enabled` false. Spp assembly at evaluate time gates on these;
            // the kernel already handles spp=0 for each gather individually.
            // Turning a term off resets RT accumulation history so the old
            // term doesn't smear — routed through the ONE existing reset path.
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_shadows"),
                label: "Shadows",
                ty: ParamType::Bool,
                default: ParamValue::Bool(true),
                range: None,
                enum_values: &[],
            },
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_ao"),
                label: "Ambient Occlusion",
                ty: ParamType::Bool,
                default: ParamValue::Bool(true),
                range: None,
                enum_values: &[],
            },
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_gi"),
                label: "Global Illumination",
                ty: ParamType::Bool,
                default: ParamValue::Bool(true),
                range: None,
                enum_values: &[],
            },
            // RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): per-scene denoiser
            // G-buffer feed toggle. Default off — an untouched scene is
            // byte-identical to before this param existed. When on, the
            // forward pass writes normals + roughness + diffuse/specular albedo
            // as MRT outputs, and the reflection pass's hit-distance is
            // upsampled to render res. Inert when the denoiser is not active
            // (DN-F wires the consumer); the flag gates the producer side only.
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_denoise_feed"),
                label: "RT Denoise Feed",
                ty: ParamType::Bool,
                default: ParamValue::Bool(false),
                range: None,
                enum_values: &[],
            },
            // RT-Stage-3 P1 (BUG-mkgh): per-scene pre-blur firefly clamp
            // toggle. Default ON = today's behavior plus the clamp; Peter can
            // flip it off per scene from the layer inspector (cardable like
            // the other rt_* toggles). Inert when RT didn't render this
            // frame or the denoiser owns the tail (`denoise_active`).
            ParamDef {
                name: std::borrow::Cow::Borrowed("rt_firefly_clamp"),
                label: "RT Firefly Clamp",
                ty: ParamType::Bool,
                default: ParamValue::Bool(true),
                range: None,
                enum_values: &[],
            },
        ];
        // Per-object TRS is no longer a param loop here — `transform_{i}`
        // (added above) carries it as a `Transform` port fed by a
        // `node.transform_3d` atom. Do NOT re-add per-object params as an unwired fallback —
        // that is the forbidden parallel-old-path shape D3 names explicitly.

        self.inputs = inputs;
        self.params = params;
        self.num_objects = objects;
        self.num_lights = lights;
        // An object-count change may reassign
        // what slot `n` means, so any previous-frame history is discarded
        // here rather than risked — the next `evaluate()` after a rebuild
        // treats every object as brand new (first-frame zero velocity),
        // exactly the same "no history yet" case a freshly-created node
        // starts in.
        self.prev_model = vec![None; n_obj];
        self.prev_positions = (0..n_obj).map(|_| None).collect();
        self.prev_instances = (0..n_obj).map(|_| None).collect();
        self.culled_instances = (0..n_obj).map(|_| None).collect();
        self.culled_prev = (0..n_obj).map(|_| None).collect();
        self.prev_view_proj = None;
        self.prev_cam_state = None;

        // Format every
        // port name exactly once, here, at rebuild time (only reached on an
        // `objects`/`lights` param change — never per frame). `evaluate()`
        // indexes these by object/light number instead of calling `format!`
        // itself.
        self.object_port_names = (0..n_obj)
            .map(|i| format!("object_{i}").into_boxed_str())
            .collect();
        self.light_port_names = (0..n_lights)
            .map(|i| format!("light_{i}").into_boxed_str())
            .collect();
    }
}
