//! Node implementations for the catalog defined in `docs/NODE_CATALOG.md`.
//!
//! This module hosts both atoms (small generic composable building blocks
//! like Mix, Feedback, Gaussian Blur) and the wrapped legacy effects
//! (Bloom, Watercolor, Halation, etc.) — all as `EffectNode` impls behind
//! flat `node.*` type IDs. The atom/effect split is presentation metadata,
//! not a structural divide.

mod abs_texture;
mod affine_transform;
mod atmosphere;
mod anti_clump_particles;
mod apply_radial_burst_to_particles;
mod array_connect_nearest;
mod array_diffuse_particles;
mod array_filter_detections;
mod array_feedback;
mod array_math;
mod array_replicate_polyline_rings;
mod array_unpack_vec2;
mod beat_gate;
mod beat_ramp;
mod bend_mesh;
mod bilateral_blur;
mod blob_detect_ffi;
mod blob_overlay_render;
mod block_displace_field;
mod bokeh_gather;
mod box_mask;
mod blur_3d_separable;
mod blinn_specular;
mod chroma_key;
mod checkerboard;
mod chromatic_displace;
mod bake_equirect_envmap;
mod basic_shape;
mod clamp_texture;
mod mirror_axis;
mod pack_channels;
mod pack_curve_xy;
mod clip_trigger_cycle;
mod clip_trigger_index;
mod coc_dilate;
mod coc_from_depth;
mod color;
mod color_sample;
mod colorize;
mod compose;
mod compressor_envelope;
mod consecutive_edges;
mod contrast;
mod convolution_2d_9tap;
mod cycle_table_row;
mod cylinder_wrap_field;
mod depth_estimate_midas;
mod digital_plants_render;
mod displace_mesh;
mod distance_to_point;
mod dither;
mod dither_pattern;
mod downsample;
mod draw_connections;
mod draw_dots;
mod draw_gauge;
mod draw_markers;
mod draw_scanlines;
mod draw_ticks;
mod edge_detect;
mod envelope_decay;
mod envelope_follower_ar;
mod fbm_per_instance;
mod field_combine;
mod film_grain;
mod filter;
mod flash;
mod flow_field_noise;
mod fract_texture;
mod fresnel_rim;
mod frequency_ratio;
mod gradient_central_diff_3d;
mod curl_slope_force_3d;
mod sample_texture_3d_at_particles;
mod simplex_noise_force_3d_at_particles;
mod diffuse_force_3d_at_particles;
mod container_repel_force_3d;
mod euler_step_particles_3d;
mod container_bounds_3d;
mod flatten_to_camera_plane;
mod apply_radial_burst_3d_to_particles;
mod scatter_particles_camera;
mod gain;
mod gaussian_blur_variable_width;
mod edges_from_grid_uv;
mod edges_from_mesh;
mod edges_from_hypercube;
mod ellipse_mask;
mod facet_normals;
mod generate_cube_mesh;
mod generate_grid_mesh;
mod generate_grid_uv;
mod generate_instance_transforms;
mod generate_range;
mod gltf_animation_source;
mod gltf_mesh_source;
mod gltf_texture_source;
mod pack_vec4;
mod gradient_central_diff;
mod gradient_ramp;
mod grid_uv_field;
mod hash_field_by_seed;
mod hdr_retention_mix;
mod hdri_source;
mod heightmap_to_normal;
mod hue_saturation;
mod hypercube_vertices;
mod image_folder;
mod instance_position_jitter;
mod instance_rotation_jitter;
mod inject_burst;
mod euler_step_particles;
mod sample_texture_at_particles;
mod wrap_particles_torus;
mod invert;
mod lambert_directional;
mod length_vec2;
mod lerp_instance_fields;
mod levels;
mod lfo;
mod lic_integrate;
mod light;
mod lightning_bolt;
mod linear_gradient;
mod luminance;
mod lut1d;
mod masked_mix;
mod matcap_two_tone;
mod math;
mod unlit_material;
mod phong_material;
mod pbr_material;
mod cel_material;
mod multi_blend;
mod mesh_ramp;
mod morph_mesh;
mod motion_blur;
mod push_along_normals;
#[cfg(all(test, feature = "gpu-proofs"))]
mod mesh_snapshot;
mod mux_array;
mod mux_scalar;
mod mux_texture;
mod neighbor_smooth;
mod nested_cubes_geometry;
mod normalize_vec2;
mod one_euro_filter;
mod optical_flow_estimate;
mod peak;
mod noise;
mod person_segment;
mod polar_field;
mod polytope_edges;
mod polytope_vertices;
mod posterize;
mod power_texture;
mod project_3d;
mod project_4d;
mod mirror_fold_uv;
mod note_rates;
mod radial_burst_force_field;
mod radial_fold_uv;
mod radial_offset_field;
mod uv_strip_clamp;
mod reinhard_tone_map;
mod remap;
mod remove_drift_3d;
mod render_3d_mesh;
mod render_instanced_3d_mesh;
pub(crate) mod render_scene;
mod render_filled_rects;
mod render_lines;
mod render_text;
mod render_value_overlay;
mod resolve_3d_accumulator;
mod resolve_accumulator;
mod rotate_3d;
mod rotate_4d;
mod rotate_vec2_by_angle;
mod sample_and_hold;
mod sample_volume_2d;
mod saturation;
mod scalar_array_accumulator;
mod scale_offset_texture;
mod scanline_jitter_field;
mod scatter_particles;
mod scatter_particles_3d;
mod seed_particles_from_texture;
mod seed_particles;
mod separable_gaussian;
mod set_alpha;
mod sharpen;
mod ssao_gtao;
mod simplex_field_2d;
mod simplex_noise_force_at_particles;
mod spawn_from_mesh;
mod scatter_on_mesh;
mod simplex_per_instance;
mod affine_scalar;
mod camera_orbit;
mod free_camera;
mod look_at_camera;
mod camera_lens;
mod canvas_area_scale;
mod centered_uv;
mod rotate_2d;
mod sin_term;
mod slope_displace;
mod texture_sum_5;
mod trig_texture;
mod smoothing;
mod smoothstep_texture;
mod track_persist;
mod temporal;
mod texture_advect;
mod texture_dimensions;
mod taper_mesh;
mod tone_map;
mod torus_wrap_field;
mod triangulate_grid;
mod tube_from_path;
// D7/P0 I6 test fixture only (docs/CINEMATIC_POST_DESIGN.md) — the whole file
// is `#![cfg(test)]`, never registered outside test builds. `pub(crate)` so
// `freeze::proof`'s I6 test can construct it directly (it is deliberately
// NOT in the global inventory-backed registry — see the module doc comment).
#[cfg(test)]
pub(crate) mod test_camera_pointwise_fixture;
mod twist_mesh;
mod trigger_ease_to;
mod trigger_gate;
mod transform_3d;
mod revolve_curve;
mod extrude_curve;
mod uv_displace_by_flow;
mod uv_field;
mod value;
mod vignette;
mod voronoi_2d;
// Crate-visible so the snapshot builder can key the `(WGSL)` header marker on
// the canonical `TYPE_ID` rather than a duplicated string literal.
pub(crate) mod wgsl_compute;
mod watercolor;
mod wet_dry_mix;

pub use abs_texture::AbsTexture;
pub use affine_transform::AffineTransform;
pub use atmosphere::AtmosphereNode;
pub use anti_clump_particles::AntiClumpParticles;
pub use apply_radial_burst_to_particles::ApplyRadialBurstToParticles;
pub use array_connect_nearest::ArrayConnectNearest;
pub use array_diffuse_particles::ArrayDiffuseParticles;
pub use array_filter_detections::ArrayFilterDetections;
pub use array_feedback::ArrayFeedback;
pub use array_math::{ARRAY_MATH_OPS, ArrayMath};
pub use array_replicate_polyline_rings::{
    ArrayReplicatePolylineRings, REPLICATE_MAX_RINGS,
};
pub use array_unpack_vec2::ArrayUnpackVec2;
pub use beat_gate::{BEAT_GATE_RATE_LABELS, BeatGate};
pub use beat_ramp::BeatRamp;
pub use blob_detect_ffi::BlobDetectFfi;
pub use blob_overlay_render::BlobOverlayRender;
pub use block_displace_field::BlockDisplaceField;
pub use box_mask::BoxMask;
pub use blinn_specular::BlinnSpecular;
pub use blur_3d_separable::{BLUR_3D_AXES, BLUR_3D_MODES, Blur3DSeparable};
pub use chroma_key::{CHROMA_KEY_MODES, ChromaKey};
pub use checkerboard::Checkerboard;
pub use chromatic_displace::ChromaticDisplace;
pub use bake_equirect_envmap::BakeEquirectEnvmap;
pub use basic_shape::{BASIC_SHAPE_SHAPES, BasicShape};
pub use clamp_texture::ClampTexture;
pub use coc_from_depth::CocFromDepth;
pub use mirror_axis::MirrorAxis;
pub use pack_channels::PackChannels;
pub use pack_curve_xy::PackCurveXy;
pub use color::{
    BRIGHTNESS_TYPE_ID, Brightness, CHANNEL_MIX_TYPE_ID, COLOR_RAMP_TYPE_ID, ChannelMix, ColorRamp,
};
pub use color_sample::ColorSample;
pub use colorize::Colorize;
pub use compose::{MIX_MODES, MIX_TYPE_ID, Mix};
pub use contrast::Contrast;
pub use compressor_envelope::{COMPRESSOR_ENVELOPE_TYPE_ID, CompressorEnvelope};
pub use consecutive_edges::{CONSECUTIVE_EDGES_MAX_CAPACITY, ConsecutiveEdges};
pub use convolution_2d_9tap::Convolution2D9Tap;
pub use cycle_table_row::CycleTableRow;
pub use cylinder_wrap_field::CylinderWrapField;
pub use depth_estimate_midas::DepthEstimateMidas;
pub use digital_plants_render::DigitalPlantsRender;
pub use displace_mesh::DisplaceMesh;
pub use draw_connections::DrawConnections;
pub use draw_dots::DrawDots;
pub use draw_gauge::DrawGauge;
pub use draw_markers::DrawMarkers;
pub use draw_scanlines::DrawScanlines;
pub use draw_ticks::DrawTicks;
pub use distance_to_point::DistanceToPoint;
pub use dither::Dither;
pub use dither_pattern::DitherPattern;
pub use edge_detect::EdgeDetect;
pub use envelope_decay::{ENVELOPE_DECAY_TYPE_ID, EnvelopeDecay};
pub use envelope_follower_ar::{ENVELOPE_FOLLOWER_AR_TYPE_ID, EnvelopeFollowerAr};
pub use fbm_per_instance::FbmPerInstance;
pub use field_combine::FieldCombine;
pub use film_grain::FilmGrain;
pub use filter::{BLUR_MODES, BLUR_TYPE_ID, Blur, THRESHOLD_TYPE_ID, Threshold};
pub use flash::{FLASH_MODES, Flash};
pub use flow_field_noise::FlowFieldNoise;
pub use fract_texture::FractTexture;
pub use fresnel_rim::FresnelRim;
pub use frequency_ratio::{FREQUENCY_RATIO_TABLE, FrequencyRatio};
pub use gradient_central_diff_3d::GradientCentralDiff3D;
pub use curl_slope_force_3d::CurlSlopeForce3D;
pub use sample_texture_3d_at_particles::SampleTexture3DAtParticles;
pub use simplex_noise_force_3d_at_particles::SimplexNoiseForce3DAtParticles;
pub use diffuse_force_3d_at_particles::DiffuseForce3DAtParticles;
pub use container_repel_force_3d::{CONTAINER_3D_MODES, ContainerRepelForce3D};
pub use euler_step_particles_3d::EulerStepParticles3D;
pub use container_bounds_3d::ContainerBounds3D;
pub use flatten_to_camera_plane::FlattenToCameraPlane;
pub use apply_radial_burst_3d_to_particles::ApplyRadialBurst3DToParticles;
pub use scatter_particles_camera::{SCATTER_CAMERA_MODES, ScatterParticlesCamera};
pub use gain::Gain;
pub use gaussian_blur_variable_width::{BLUR_VARIABLE_AXES, GaussianBlurVariableWidth};
pub use edges_from_grid_uv::EdgesFromGridUv;
pub use edges_from_mesh::EdgesFromMesh;
pub use edges_from_hypercube::EdgesFromHypercube;
pub use ellipse_mask::EllipseMask;
pub use generate_cube_mesh::{CUBE_VERTEX_COUNT, GenerateCubeMesh};
pub use generate_grid_mesh::GenerateGridMesh;
pub use generate_grid_uv::{
    GRID_UV_DEFAULT_SIZE, GRID_UV_MAX_SIZE, GenerateGridUv,
};
pub use generate_instance_transforms::{
    GenerateInstanceTransforms, INSTANCE_LAYOUTS,
};
pub use generate_range::GenerateRange;
pub use gltf_animation_source::GltfAnimationSource;
pub use gltf_mesh_source::GltfMeshSource;
pub use gltf_texture_source::GltfTextureSource;
pub use pack_vec4::PackVec4;
pub use gradient_central_diff::{GRADIENT_CHANNELS, GradientCentralDiff};
pub use gradient_ramp::GradientRamp;
pub use grid_uv_field::GridUvField;
pub use hash_field_by_seed::{HASH_FIELD_MODES, HashFieldBySeed};
pub use hdri_source::HdriSource;
pub use heightmap_to_normal::HeightmapToNormal;
pub use image_folder::ImageFolder;
pub use instance_position_jitter::InstancePositionJitter;
pub use instance_rotation_jitter::InstanceRotationJitter;
pub use inject_burst::{INJECT_BURST_TYPE_ID, InjectBurst};
pub use euler_step_particles::EulerStepParticles;
pub use sample_texture_at_particles::SampleTextureAtParticles;
pub use wrap_particles_torus::WrapParticlesTorus;
pub use hue_saturation::HueSaturation;
pub use hypercube_vertices::HypercubeVertices;
pub use invert::Invert;
pub use lambert_directional::LambertDirectional;
pub use length_vec2::LengthVec2;
pub use lerp_instance_fields::LerpInstanceFields;
pub use levels::Levels;
pub use lfo::{LFO_RATE_LABELS, LFO_SHAPES, Lfo};
pub use lic_integrate::LicIntegrate;
pub use light::LightNode;
pub use linear_gradient::LinearGradient;
pub use luminance::Luminance;
pub use lut1d::ColorLut;
pub use math::{MATH_OPS, Math};
pub use masked_mix::MaskedMix;
pub use matcap_two_tone::MatcapTwoTone;
pub use unlit_material::UnlitMaterial;
pub use phong_material::PhongMaterial;
pub use pbr_material::PbrMaterial;
pub use cel_material::CelMaterial;
pub use multi_blend::MultiBlend;
pub use mux_array::MuxArray;
pub use mux_scalar::MuxScalar;
pub use mux_texture::MuxTexture;
pub use neighbor_smooth::NeighborSmooth;
pub use nested_cubes_geometry::{NESTED_CUBES_INSTANCE_COUNT, NestedCubesGeometry};
pub use normalize_vec2::NormalizeVec2;
pub use one_euro_filter::OneEuroFilter;
pub use optical_flow_estimate::OpticalFlowEstimate;
pub use peak::Peak;
pub use noise::Noise;
pub use person_segment::PersonSegment;
pub use polar_field::PolarField;
pub use polytope_edges::PolytopeEdges;
pub use polytope_vertices::PolytopeVertices;
pub use posterize::Posterize;
pub use power_texture::PowerTexture;
pub use project_3d::{PROJECT_3D_MODES, Project3D};
pub use project_4d::Project4D;
pub use mirror_fold_uv::{MIRROR_FOLD_MODES, MirrorFoldUv};
pub use note_rates::{NOTE_RATE_LABELS, NOTE_RATE_VALUES};
pub use radial_burst_force_field::RadialBurstForceField;
pub use radial_fold_uv::RadialFoldUv;
pub use radial_offset_field::RadialOffsetField;
pub use uv_strip_clamp::{UV_STRIP_CLAMP_MODES, UvStripClamp};
pub use reinhard_tone_map::ReinhardToneMap;
pub use remap::{REMAP_WRAP_MODES, Remap};
pub use remove_drift_3d::RemoveDrift3D;
pub use render_3d_mesh::Render3DMesh;
pub use render_instanced_3d_mesh::RenderInstanced3DMesh;
pub use render_scene::RenderScene;
pub use render_filled_rects::RenderFilledRects;
pub use render_lines::RenderLines;
pub use render_text::RenderText;
pub use render_value_overlay::RenderValueOverlay;
pub use resolve_3d_accumulator::Resolve3DAccumulator;
pub use resolve_accumulator::ResolveAccumulator;
pub use rotate_3d::Rotate3D;
pub use rotate_4d::Rotate4D;
pub use rotate_vec2_by_angle::RotateVec2ByAngle;
pub use clip_trigger_cycle::ClipTriggerCycleNode;
pub use sample_and_hold::{SAMPLE_AND_HOLD_TYPE_ID, SampleAndHold};
pub use sample_volume_2d::SampleVolume2D;
pub use saturation::Saturation;
pub use scalar_array_accumulator::ScalarArrayAccumulator;
pub use scale_offset_texture::ScaleOffsetTexture;
pub use scanline_jitter_field::ScanlineJitterField;
pub use scatter_on_mesh::ScatterOnMesh;
pub use scatter_particles::ScatterParticles;
pub use scatter_particles_3d::ScatterParticles3D;
pub use seed_particles_from_texture::SeedParticlesFromTexture;
pub use seed_particles::SeedParticles;
pub use separable_gaussian::{
    GAUSSIAN_BLUR_AXES, GAUSSIAN_BLUR_KERNELS, GAUSSIAN_BLUR_TYPE_ID, GaussianBlur,
};
pub use sharpen::Sharpen;
pub use simplex_field_2d::{SIMPLEX_FIELD_OUTPUT_CHANNELS, SimplexField2D};
pub use simplex_noise_force_at_particles::SimplexNoiseForceAtParticles;
pub use simplex_per_instance::SimplexPerInstance;
pub use affine_scalar::AffineScalar;
pub use camera_orbit::{CameraOrbit, DEFAULT_NEAR};
pub use free_camera::FreeCamera;
pub use look_at_camera::LookAtCamera;
pub use camera_lens::CameraLens;
pub use canvas_area_scale::CanvasAreaScale;
pub use centered_uv::CenteredUv;
pub use rotate_2d::Rotate2D;
pub use sin_term::SinTerm;
pub use slope_displace::SlopeDisplace;
pub use texture_sum_5::TextureSum5;
pub use trig_texture::{TRIG_MODES, TrigTexture};
pub use smoothing::{SMOOTHING_TYPE_ID, Smoothing};
pub use smoothstep_texture::SmoothstepTexture;
pub use temporal::{FEEDBACK_TYPE_ID, Feedback};
pub use texture_advect::{TEXTURE_ADVECT_BOUNDARIES, TextureAdvect};
pub use texture_dimensions::TextureDimensions;
pub use tone_map::{TONE_MAP_CURVES, TONE_MAP_MODES, ToneMap};
pub use torus_wrap_field::TorusWrapField;
pub use triangulate_grid::TriangulateGrid;
pub use trigger_ease_to::{TRIGGER_EASE_TO_TYPE_ID, TriggerEaseTo};
pub use track_persist::TrackPersist;
pub use trigger_gate::TriggerGate;
pub use transform_3d::Transform3D;
pub use uv_displace_by_flow::UvDisplaceByFlow;
pub use uv_field::UvField;
pub use value::Value;
pub use vignette::{VIGNETTE_SHAPES, Vignette};
pub use voronoi_2d::Voronoi2D;
pub use wgsl_compute::{DEFAULT_WGSL as DEFAULT_WGSL_COMPUTE, WgslCompute};
pub use watercolor::{WATERCOLOR_TYPE_ID, Watercolor};
pub use wet_dry_mix::{WET_DRY_TYPE_ID, WetDry};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{
        EffectNode, Executor, FinalOutput, FrameTime, Graph, ParamType, ParamValue, Source,
        compile, validate,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Iterate one boxed instance of each V1 primitive so tests can assert
    /// invariants over the whole catalog without listing them by hand.
    fn all_primitives() -> Vec<Box<dyn EffectNode>> {
        vec![
            Box::new(Brightness::new()),
            Box::new(ChannelMix::new()),
            Box::new(ColorRamp::new()),
            Box::new(Mix::new()),
            Box::new(Threshold::new()),
            Box::new(Blur::new()),
            Box::new(Feedback::new()),
            Box::new(WetDry::new()),
        ]
    }

    #[test]
    fn all_v1_primitives_have_unique_type_ids() {
        let primitives = all_primitives();
        let ids: HashSet<&str> = primitives.iter().map(|p| p.type_id().as_str()).collect();
        assert_eq!(ids.len(), 8, "primitive type IDs must be unique");
    }

    #[test]
    fn all_v1_primitive_type_ids_have_node_prefix() {
        for p in all_primitives() {
            assert!(
                p.type_id().as_str().starts_with("node."),
                "node type IDs must start with `node.` — got {}",
                p.type_id().as_str()
            );
        }
    }

    #[test]
    fn all_v1_primitives_produce_at_least_one_output() {
        for p in all_primitives() {
            assert!(
                !p.outputs().is_empty(),
                "primitive {} has no outputs",
                p.type_id().as_str()
            );
        }
    }

    #[test]
    fn parameter_defaults_match_declared_types() {
        // Catches typos like `default: ParamValue::Float(...)` on a
        // `ty: ParamType::Vec3` parameter.
        //
        // `ParamType::Table` is allowed to declare a `Float(_)` sentinel
        // default — Tables can't live in static-const `ParamValue` (Arc
        // isn't const-constructible), so primitives that take a Table
        // param ship a placeholder that's overridden by the JSON preset.
        for p in all_primitives() {
            for def in p.parameters() {
                let ok = matches!(
                    (def.ty, &def.default),
                    (ParamType::Float, ParamValue::Float(_))
                        | (ParamType::Int, ParamValue::Float(_))
                        | (ParamType::Bool, ParamValue::Bool(_))
                        | (ParamType::Vec2, ParamValue::Vec2(_))
                        | (ParamType::Vec3, ParamValue::Vec3(_))
                        | (ParamType::Vec4, ParamValue::Vec4(_))
                        | (ParamType::Color, ParamValue::Color(_))
                        | (ParamType::Enum, ParamValue::Enum(_))
                        | (ParamType::Table, ParamValue::Float(_))
                        | (ParamType::Table, ParamValue::Table(_))
                        | (ParamType::Trigger, ParamValue::Float(_))
                );
                assert!(
                    ok,
                    "{} param `{}`: default {:?} does not match declared type {:?}",
                    p.type_id().as_str(),
                    def.name,
                    def.default,
                    def.ty,
                );
            }
        }
    }

    /// Regression test for the `scalar_or_param` Int fall-through bug.
    ///
    /// Before the `ParamValue::Int` → `Float` storage collapse, an
    /// `Int`-typed param wired into a primitive via JSON preset
    /// (`{"type":"Int","value":N}`) would deserialize to
    /// `ParamValue::Int(N)` and silently fall through every reader that
    /// only matched on `Float` — the slider moved, the visual didn't.
    ///
    /// The new contract: an `Int`-typed param's default lives in
    /// `ParamValue::Float`, *and* the value can be set via
    /// `ParamValue::Float(n as f32)` and read back via the standard
    /// scalar-coercion helpers without losing the value. This test
    /// asserts the contract at the primitive-registry level so any
    /// future primitive that declares `ty: ParamType::Int` is forced
    /// onto the safe path.
    #[test]
    fn int_typed_params_use_float_storage_and_coerce_cleanly() {
        use crate::node_graph::parameters::ParamValue;
        for p in all_primitives() {
            for def in p.parameters() {
                if def.ty != ParamType::Int {
                    continue;
                }
                // Default storage must be Float.
                let ParamValue::Float(default_f) = def.default else {
                    panic!(
                        "{} param `{}`: Int-typed param must store its default \
                         in ParamValue::Float (collapsed numeric storage); got {:?}",
                        p.type_id().as_str(),
                        def.name,
                        def.default,
                    );
                };
                // Float storage must round-trip cleanly through the
                // scalar coercion helper — this is the helper that
                // `scalar_or_param` and every primitive read site
                // funnels through.
                let coerced = def.default.as_scalar().unwrap_or_else(|| {
                    panic!(
                        "{} param `{}`: Float-stored Int default did not \
                         coerce via as_scalar()",
                        p.type_id().as_str(),
                        def.name,
                    )
                });
                assert_eq!(
                    coerced, default_f,
                    "{} param `{}`: as_scalar() must return the stored f32",
                    p.type_id().as_str(),
                    def.name,
                );
            }
        }
    }

    #[test]
    fn enum_param_defaults_are_in_range() {
        for p in all_primitives() {
            for def in p.parameters() {
                if def.ty == ParamType::Enum {
                    let ParamValue::Enum(idx) = def.default else {
                        unreachable!("enforced by parameter_defaults_match_declared_types");
                    };
                    assert!(
                        (idx as usize) < def.enum_values.len(),
                        "{} param `{}`: default index {} out of bounds for {} options",
                        p.type_id().as_str(),
                        def.name,
                        idx,
                        def.enum_values.len(),
                    );
                }
            }
        }
    }

    /// Integration test: assemble the decomposed Bloom shape (blur a
    /// copy of the source, mix it back) from primitives + boundary
    /// nodes, compile it, execute it. Validates that the trait shape and
    /// pool work for a real multi-node graph with source fan-out and a
    /// multi-input node. Mirrors how Bloom.json is built today
    /// (threshold → downsample → blur → mix), minus the prefilter.
    ///
    /// Topology:
    ///
    /// ```text
    ///   Source ──→ Blur ──→ Mix.b ─→ FinalOutput
    ///       └─────────────→ Mix.a
    /// ```
    #[test]
    fn decomposed_bloom_shape_compiles_and_executes() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let blur = g.add_node(Box::new(Blur::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (blur, "source")).unwrap();
        g.connect((src, "out"), (mix, "a")).unwrap();
        g.connect((blur, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 4);

        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    /// Mix has two required inputs; both must be wired or validate() fails.
    #[test]
    fn mix_requires_both_inputs_to_be_wired() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let _mix = g.add_node(Box::new(Mix::new()));
        // Don't wire either of mix's inputs.
        assert!(matches!(
            validate(&g),
            Err(crate::node_graph::GraphError::RequiredInputUnwired { .. })
        ));
    }

    /// Every primitive that declares an `Array<T>` output port must
    /// know how to size that output via
    /// [`EffectNode::array_output_capacity`] — either from a node-local
    /// param, from a same-as-input passthrough, or computed from
    /// multiple params. Test verifies the contract is *resolvable*
    /// from defaults: with the primitive's `parameters()` defaults
    /// installed AND assuming any Array input was bound at a generous
    /// upper bound, the method must return `Some(_)`.
    ///
    /// Why this matters: the chain build / `JsonGraphGenerator`
    /// pre-allocator reads this method on every Array-producing node
    /// at construction. If it returns `None`, the buffer is never
    /// allocated, downstream renders nothing — the Lissajous
    /// black-frame bug class (commit 23e440aa). This test promotes
    /// the contract from "convention you can forget" to "CI-enforced
    /// invariant" across every primitive, including future ones.
    ///
    /// Walks the live [`super::super::PrimitiveRegistry`] so new
    /// primitives are picked up automatically — no central list to
    /// maintain.
    #[test]
    fn every_array_output_declares_a_valid_capacity_source() {
        use super::super::PrimitiveRegistry;
        use super::super::ports::PortType;
        use ahash::AHashMap;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        for type_id in registry.known_type_ids() {
            let Some(node) = registry.construct(type_id) else {
                continue;
            };
            // Synthesize a default-param bag matching `parameters()`.
            let mut params: AHashMap<std::borrow::Cow<'static, str>, ParamValue> =
                AHashMap::default();
            for def in node.parameters() {
                params.insert(def.name.clone(), def.default.clone());
            }
            // Pretend every Array input was bound at a large but finite
            // capacity. Same-as-input transforms should resolve against
            // this; producers should ignore it.
            let mut synthetic_inputs: Vec<(&str, u32)> = Vec::new();
            for port in node.inputs() {
                if matches!(port.ty, PortType::Array(_)) {
                    synthetic_inputs.push((port.name.as_ref(), 1024));
                }
            }

            // Canvas-sized outputs bypass `array_output_capacity` —
            // the chain builder sizes them from the backend's canvas
            // dims at allocation time. They're a valid capacity
            // source even when the method returns None.
            let canvas_sized: std::collections::HashSet<&str> =
                node.canvas_sized_array_outputs().iter().copied().collect();

            for port in node.outputs() {
                if !matches!(port.ty, PortType::Array(_)) {
                    continue;
                }
                if canvas_sized.contains(port.name.as_ref()) {
                    continue;
                }
                let cap = node.array_output_capacity(port.name.as_ref(), &params, &synthetic_inputs);
                if cap.is_none() {
                    violations.push(format!(
                        "{type_id}: Array output `{}` — \
                         array_output_capacity returned None with default \
                         params and Array inputs bound at 1024. Override \
                         the method on the primitive, declare the port via \
                         `canvas_sized_array_outputs()` for canvas-matched \
                         buffers, or for producers add a `max_capacity` \
                         param with an Int/Float default.",
                        port.name,
                    ));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "Array-output capacity invariant violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// Every shipping primitive's Array ports must carry a declared
    /// Channels signature ([`ArrayType::specs`] non-empty). The
    /// signature is what makes wire validation refuse to connect
    /// byte-identical buffers whose conventions don't match —
    /// `CurvePoint` (channels `x, y`) vs `EdgePair` (channels
    /// `a_index, b_index`) are both 8/4 and would have connected
    /// silently under a pure size/align check. With named channels
    /// they don't.
    ///
    /// Empty-specs Array ports are the deliberate opt-out for
    /// genuinely untyped raw-byte buffers (escape-hatch nodes,
    /// scratch state). Allowed for `node.wgsl_compute*` (the wire
    /// shape derives from user WGSL via naga — `_pad*` fields skip,
    /// matrices and runtime arrays fall back to empty specs) and the
    /// `node.__smoke_test_*` fixtures. Anywhere else it's a CI
    /// failure pointing at a missing `KnownItem::SPECS` or a missing
    /// inline `Channels[…]` declaration.
    ///
    /// Walks the live [`super::super::PrimitiveRegistry`] so new
    /// primitives are picked up automatically.
    #[test]
    fn every_conventional_array_port_declares_a_channels_signature() {
        use super::super::PrimitiveRegistry;
        use super::super::ports::PortType;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        for type_id in registry.known_type_ids() {
            // Carve-outs (see doc comment above for rationale).
            if type_id.starts_with("node.wgsl_compute")
                || type_id.starts_with("node.__smoke_test_")
                || type_id.starts_with("system.")
            {
                continue;
            }

            let Some(node) = registry.construct(type_id) else {
                continue;
            };

            let mut check_port = |kind_label: &str, port_name: &str, ty: &PortType| {
                if let PortType::Array(layout) = ty
                    && layout.specs.is_empty()
                {
                    violations.push(format!(
                        "{type_id}: {kind_label} `{port_name}` is Array<…> \
                         with no Channels signature (specs is empty). \
                         Declare the port via `Array(T)` (with a \
                         `KnownItem` impl on T that sets `SPECS`), via \
                         inline `Channels[name: Type, …]` syntax, or — \
                         if the buffer is genuinely untyped scratch — \
                         extend this test's carve-out list.",
                    ));
                }
            };

            for port in node.inputs() {
                check_port("input", port.name.as_ref(), &port.ty);
            }
            for port in node.outputs() {
                check_port("output", port.name.as_ref(), &port.ty);
            }
        }
        assert!(
            violations.is_empty(),
            "Array-port Channels-signature invariant violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// Param values can be set on a primitive instance through the Graph API.
    #[test]
    fn primitive_params_accept_typed_overrides() {
        let mut g = Graph::new();
        let id = g.add_node(Box::new(Threshold::new()));
        g.set_param(id, "level", ParamValue::Float(0.7)).unwrap();
        g.set_param(id, "softness", ParamValue::Float(0.1)).unwrap();
        // Unknown param is rejected.
        assert!(g.set_param(id, "missing", ParamValue::Float(0.0)).is_err());
    }
}
