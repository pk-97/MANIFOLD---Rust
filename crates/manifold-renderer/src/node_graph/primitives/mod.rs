//! Node implementations for the catalog defined in `docs/NODE_CATALOG.md`.
//!
//! This module hosts both atoms (small generic composable building blocks
//! like Mix, Feedback, Gaussian Blur) and the wrapped legacy effects
//! (Bloom, Watercolor, Halation, etc.) — all as `EffectNode` impls behind
//! flat `node.*` type IDs. The atom/effect split is presentation metadata,
//! not a structural divide.

mod abs_texture;
mod affine_transform;
mod anti_clump_particles;
mod apply_radial_burst_to_particles;
mod array_diffuse_particles;
mod array_feedback;
mod array_math;
mod array_replicate_polyline_rings;
mod array_unpack_vec2;
mod auto_gain;
mod auto_gain_apply;
mod beat_gate;
mod blob_detect_ffi;
mod blob_overlay_render;
mod blob_tracking;
mod bloom;
mod blur_3d_separable;
mod blinn_specular;
mod chroma_key;
mod checkerboard;
mod chromatic_displace;
mod chromatic_offset;
mod clamp_stretch;
mod bake_equirect_envmap;
mod cast_array;
mod clamp_texture;
mod cook_torrance_specular;
mod equirect_envmap_sample;
mod mirror_axis;
mod pack_channels;
mod pack_curve_xy;
mod clip_trigger_cycle;
mod clip_trigger_index;
mod color;
mod color_grade;
mod color_sample;
mod compose;
mod consecutive_edges;
mod convolution_2d_9tap;
mod cycle_table_row;
mod cylinder_wrap_field;
mod depth_estimate_midas;
mod depth_of_field;
mod digital_plants_render;
mod displace_mesh;
mod distance_to_point;
mod dither_pattern;
mod downsample;
mod edge_detect;
mod envelope_decay;
mod envelope_follower_ar;
mod fbm_2d;
mod fbm_per_instance;
mod field_combine;
mod filter;
mod flow_field_noise;
mod fract_texture;
mod fresnel_rim;
mod frequency_ratio;
mod fluid_gradient_curl_3d;
mod fluid_seed_3d;
mod fluid_simulate_3d;
mod scatter_particles_camera;
mod gain;
mod gaussian_blur_variable_width;
mod edges_from_grid_uv;
mod generate_cube_mesh;
mod generate_grid_mesh;
mod generate_grid_uv;
mod generate_instance_transforms;
mod generate_range;
mod generate_tesseract_vertices;
mod pack_vec4;
mod glitch;
mod gradient_central_diff;
mod grid_uv_field;
mod halation;
mod hash_noise_field_2d;
mod heightmap_to_normal;
mod highlight_boost;
mod image_folder;
mod infrared;
mod instance_position_jitter;
mod instance_rotation_jitter;
mod inject_burst;
mod euler_step_particles;
mod sample_texture_at_particles;
mod wrap_particles_torus;
mod invert;
mod kaleido_fold;
mod lambert_directional;
mod length_vec2;
mod lerp_instance_fields;
mod levels;
mod lfo;
mod lic_integrate;
mod luminance;
mod lut1d;
mod masked_mix;
mod matcap_two_tone;
mod math;
mod mux_array;
mod mux_scalar;
mod mux_texture;
mod neighbor_smooth;
mod nested_cubes_geometry;
mod normalize_vec2;
mod optical_flow_estimate;
mod peak;
mod perlin_noise_2d;
mod polar_field;
mod polytope_edges;
mod polytope_vertices;
mod power_texture;
mod project_3d;
mod project_4d;
mod quad_mirror;
mod radial_burst_force_field;
mod reinhard_tone_map;
mod render_3d_mesh;
mod render_instanced_3d_mesh;
mod render_lines;
mod render_text;
mod resolve_3d_accumulator;
mod resolve_accumulator;
mod rotate_3d;
mod rotate_4d;
mod rotate_vec2_by_angle;
mod sample_and_hold;
mod sample_volume_2d;
mod scalar_array_accumulator;
mod scale_offset_texture;
mod scatter_particles;
mod scatter_particles_3d;
mod seed_particles_from_texture;
mod seed_particles;
mod separable_gaussian;
mod sharpen;
mod simplex_field_2d;
mod simplex_noise_2d;
mod simplex_noise_force_at_particles;
mod simplex_per_instance;
mod affine_scalar;
mod camera_orbit;
mod canvas_area_scale;
mod centered_uv;
mod plasma_pattern_2d;
mod rotate_2d;
mod star_field_2d;
mod shape_2d;
mod sin_term;
mod texture_sum_5;
mod trig_texture;
mod smoothing;
mod smoothstep_texture;
mod strobe;
mod temporal;
mod texture_advect;
mod tone_map;
mod torus_wrap_field;
mod triangulate_grid;
mod trigger_gate;
mod uv;
mod uv_displace_by_flow;
mod uv_field;
mod value;
mod vignette;
mod voronoi_2d;
mod voronoi_prism;
mod wgsl_compute;
mod wgsl_compute_0in_1tex;
mod wgsl_compute_1tex_1tex;
mod wgsl_compute_2tex_1tex;
mod watercolor;
mod wet_dry_mix;
mod wireframe_depth;

pub use abs_texture::AbsTexture;
pub use affine_transform::AffineTransform;
pub use anti_clump_particles::AntiClumpParticles;
pub use apply_radial_burst_to_particles::ApplyRadialBurstToParticles;
pub use array_diffuse_particles::ArrayDiffuseParticles;
pub use array_feedback::ArrayFeedback;
pub use array_math::{ARRAY_MATH_OPS, ArrayMath};
pub use array_replicate_polyline_rings::{
    ArrayReplicatePolylineRings, REPLICATE_MAX_RINGS,
};
pub use array_unpack_vec2::ArrayUnpackVec2;
pub use auto_gain::{AUTO_GAIN_CHARACTERS, AUTO_GAIN_TYPE_ID, AutoGain};
pub use auto_gain_apply::AutoGainApply;
pub use beat_gate::{BEAT_GATE_RATE_LABELS, BeatGate};
pub use blob_detect_ffi::BlobDetectFfi;
pub use blob_overlay_render::BlobOverlayRender;
pub use blob_tracking::{BLOB_TRACKING_TYPE_ID, BlobTracking};
pub use bloom::{BLOOM_TYPE_ID, Bloom};
pub use blinn_specular::BlinnSpecular;
pub use blur_3d_separable::{BLUR_3D_AXES, BLUR_3D_MODES, Blur3DSeparable};
pub use chroma_key::{CHROMA_KEY_MODES, ChromaKey};
pub use checkerboard::Checkerboard;
pub use chromatic_displace::ChromaticDisplace;
pub use chromatic_offset::ChromaticOffset;
pub use clamp_stretch::ClampStretch;
pub use bake_equirect_envmap::BakeEquirectEnvmap;
pub use cast_array::{
    CastAsCurvePoint, CastAsEdgePair, CastAsInstanceTransform, CastAsMeshVertex, CastAsParticle,
    CastAsU32,
};
pub use clamp_texture::ClampTexture;
pub use cook_torrance_specular::CookTorranceSpecular;
pub use equirect_envmap_sample::EquirectEnvmapSample;
pub use mirror_axis::MirrorAxis;
pub use pack_channels::PackChannels;
pub use pack_curve_xy::PackCurveXy;
pub use color::{
    BRIGHTNESS_TYPE_ID, Brightness, CHANNEL_MIX_TYPE_ID, COLOR_RAMP_TYPE_ID, ChannelMix, ColorRamp,
};
pub use color_grade::ColorGrade;
pub use color_sample::ColorSample;
pub use compose::{BLEND_MODES, BLEND_TYPE_ID, Blend, MIX_MODES, MIX_TYPE_ID, Mix};
pub use consecutive_edges::{CONSECUTIVE_EDGES_MAX_CAPACITY, ConsecutiveEdges};
pub use convolution_2d_9tap::Convolution2D9Tap;
pub use cycle_table_row::CycleTableRow;
pub use cylinder_wrap_field::CylinderWrapField;
pub use depth_estimate_midas::DepthEstimateMidas;
pub use depth_of_field::{
    DEPTH_OF_FIELD_FOCUS_MODES, DEPTH_OF_FIELD_QUALITIES, DEPTH_OF_FIELD_TYPE_ID, DepthOfField,
};
pub use digital_plants_render::DigitalPlantsRender;
pub use displace_mesh::DisplaceMesh;
pub use distance_to_point::DistanceToPoint;
pub use dither_pattern::DitherPattern;
pub use edge_detect::EdgeDetect;
pub use envelope_decay::{ENVELOPE_DECAY_TYPE_ID, EnvelopeDecay};
pub use envelope_follower_ar::{ENVELOPE_FOLLOWER_AR_TYPE_ID, EnvelopeFollowerAr};
pub use fbm_2d::Fbm2D;
pub use fbm_per_instance::FbmPerInstance;
pub use field_combine::FieldCombine;
pub use filter::{
    BLUR_MODES, BLUR_TYPE_ID, Blur, MIP_CHAIN_TYPE_ID, MipChain, THRESHOLD_TYPE_ID, Threshold,
};
pub use flow_field_noise::FlowFieldNoise;
pub use fract_texture::FractTexture;
pub use fresnel_rim::FresnelRim;
pub use frequency_ratio::{FREQUENCY_RATIO_TABLE, FrequencyRatio};
pub use fluid_gradient_curl_3d::FluidGradientCurl3D;
pub use fluid_seed_3d::{
    FLUID_SEED_3D_CONTAINER_MODES, FLUID_SEED_3D_PATTERNS, FluidSeed3D,
};
pub use fluid_simulate_3d::{FLUID_3D_CONTAINER_MODES, FluidSimulate3D};
pub use scatter_particles_camera::{SCATTER_CAMERA_MODES, ScatterParticlesCamera};
pub use gain::Gain;
pub use gaussian_blur_variable_width::{BLUR_VARIABLE_AXES, GaussianBlurVariableWidth};
pub use edges_from_grid_uv::EdgesFromGridUv;
pub use generate_cube_mesh::{CUBE_VERTEX_COUNT, GenerateCubeMesh};
pub use generate_grid_mesh::GenerateGridMesh;
pub use generate_grid_uv::{
    GRID_UV_DEFAULT_SIZE, GRID_UV_MAX_SIZE, GenerateGridUv,
};
pub use generate_instance_transforms::{
    GenerateInstanceTransforms, INSTANCE_LAYOUTS,
};
pub use generate_range::GenerateRange;
pub use generate_tesseract_vertices::{
    GenerateTesseractVertices, TESSERACT_VERTEX_COUNT,
};
pub use pack_vec4::PackVec4;
pub use glitch::Glitch;
pub use gradient_central_diff::{GRADIENT_CHANNELS, GradientCentralDiff};
pub use grid_uv_field::GridUvField;
pub use halation::{HALATION_TYPE_ID, Halation};
pub use hash_noise_field_2d::HashNoiseField2D;
pub use heightmap_to_normal::HeightmapToNormal;
pub use highlight_boost::HighlightBoost;
pub use image_folder::ImageFolder;
pub use infrared::{INFRARED_PALETTES, INFRARED_TYPE_ID, Infrared};
pub use instance_position_jitter::InstancePositionJitter;
pub use instance_rotation_jitter::InstanceRotationJitter;
pub use inject_burst::{INJECT_BURST_TYPE_ID, InjectBurst};
pub use euler_step_particles::EulerStepParticles;
pub use sample_texture_at_particles::SampleTextureAtParticles;
pub use wrap_particles_torus::WrapParticlesTorus;
pub use invert::Invert;
pub use kaleido_fold::KaleidoFold;
pub use lambert_directional::LambertDirectional;
pub use length_vec2::LengthVec2;
pub use lerp_instance_fields::LerpInstanceFields;
pub use levels::Levels;
pub use lfo::{LFO_RATE_LABELS, LFO_SHAPES, Lfo};
pub use lic_integrate::LicIntegrate;
pub use luminance::Luminance;
pub use lut1d::ColorLut;
pub use math::{MATH_OPS, Math};
pub use masked_mix::MaskedMix;
pub use matcap_two_tone::MatcapTwoTone;
pub use mux_array::MuxArray;
pub use mux_scalar::MuxScalar;
pub use mux_texture::MuxTexture;
pub use neighbor_smooth::NeighborSmooth;
pub use nested_cubes_geometry::{NESTED_CUBES_INSTANCE_COUNT, NestedCubesGeometry};
pub use normalize_vec2::NormalizeVec2;
pub use optical_flow_estimate::OpticalFlowEstimate;
pub use peak::Peak;
pub use perlin_noise_2d::PerlinNoise2D;
pub use polar_field::PolarField;
pub use polytope_edges::PolytopeEdges;
pub use polytope_vertices::PolytopeVertices;
pub use power_texture::PowerTexture;
pub use project_3d::{PROJECT_3D_MODES, Project3D};
pub use project_4d::Project4D;
pub use quad_mirror::{QUAD_MIRROR_TYPE_ID, QuadMirror};
pub use radial_burst_force_field::RadialBurstForceField;
pub use reinhard_tone_map::ReinhardToneMap;
pub use render_3d_mesh::Render3DMesh;
pub use render_instanced_3d_mesh::RenderInstanced3DMesh;
pub use render_lines::RenderLines;
pub use render_text::RenderText;
pub use resolve_3d_accumulator::Resolve3DAccumulator;
pub use resolve_accumulator::ResolveAccumulator;
pub use rotate_3d::Rotate3D;
pub use rotate_4d::Rotate4D;
pub use rotate_vec2_by_angle::RotateVec2ByAngle;
pub use clip_trigger_cycle::ClipTriggerCycleNode;
pub use sample_and_hold::{SAMPLE_AND_HOLD_TYPE_ID, SampleAndHold};
pub use sample_volume_2d::SampleVolume2D;
pub use scalar_array_accumulator::ScalarArrayAccumulator;
pub use scale_offset_texture::ScaleOffsetTexture;
pub use scatter_particles::ScatterParticles;
pub use scatter_particles_3d::ScatterParticles3D;
pub use seed_particles_from_texture::SeedParticlesFromTexture;
pub use seed_particles::SeedParticles;
pub use separable_gaussian::{
    GAUSSIAN_BLUR_AXES, GAUSSIAN_BLUR_KERNELS, GAUSSIAN_BLUR_TYPE_ID, GaussianBlur,
};
pub use sharpen::Sharpen;
pub use simplex_field_2d::{SIMPLEX_FIELD_OUTPUT_CHANNELS, SimplexField2D};
pub use simplex_noise_2d::SimplexNoise2D;
pub use simplex_noise_force_at_particles::SimplexNoiseForceAtParticles;
pub use simplex_per_instance::SimplexPerInstance;
pub use affine_scalar::AffineScalar;
pub use camera_orbit::CameraOrbit;
pub use canvas_area_scale::CanvasAreaScale;
pub use centered_uv::CenteredUv;
pub use plasma_pattern_2d::{PLASMA_PATTERNS, PLASMA_PATTERN_COUNT, PlasmaPattern2D};
pub use rotate_2d::Rotate2D;
pub use star_field_2d::{STAR_FIELD_TOTAL_STARS, StarField2D};
pub use shape_2d::{SHAPE_2D_FILL_MODES, Shape2D};
pub use sin_term::SinTerm;
pub use texture_sum_5::TextureSum5;
pub use trig_texture::{TRIG_MODES, TrigTexture};
pub use smoothing::{SMOOTHING_TYPE_ID, Smoothing};
pub use smoothstep_texture::SmoothstepTexture;
pub use strobe::{
    NOTE_RATE_LABELS, NOTE_RATE_VALUES, NOTE_RATES as STROBE_NOTE_RATES, Strobe,
};
pub use temporal::{FEEDBACK_TYPE_ID, Feedback};
pub use texture_advect::{TEXTURE_ADVECT_BOUNDARIES, TextureAdvect};
pub use tone_map::{TONE_MAP_CURVES, TONE_MAP_MODES, ToneMap};
pub use torus_wrap_field::TorusWrapField;
pub use triangulate_grid::TriangulateGrid;
pub use trigger_gate::TriggerGate;
pub use uv::{
    SAMPLE_FILTER_MODES, SAMPLE_TYPE_ID, SAMPLE_WRAP_MODES, Sample, TRANSFORM_MODES,
    TRANSFORM_TYPE_ID, Transform,
};
pub use uv_displace_by_flow::UvDisplaceByFlow;
pub use uv_field::UvField;
pub use value::Value;
pub use vignette::{VIGNETTE_SHAPES, Vignette};
pub use voronoi_2d::Voronoi2D;
pub use voronoi_prism::VoronoiPrism;
pub use wgsl_compute::{DEFAULT_WGSL as DEFAULT_WGSL_COMPUTE, WgslCompute};
pub use wgsl_compute_0in_1tex::{DEFAULT_WGSL_0IN_1TEX, WgslCompute0In1Tex};
pub use wgsl_compute_1tex_1tex::{DEFAULT_WGSL_1TEX_1TEX, WgslCompute1Tex1Tex};
pub use wgsl_compute_2tex_1tex::{DEFAULT_WGSL_2TEX_1TEX, WgslCompute2Tex1Tex};
pub use watercolor::{WATERCOLOR_TYPE_ID, Watercolor};
pub use wet_dry_mix::{WET_DRY_TYPE_ID, WetDry};
pub use wireframe_depth::{
    WIREFRAME_DEPTH_BLEND_MODES, WIREFRAME_DEPTH_MESH_RATES, WIREFRAME_DEPTH_ONOFF,
    WIREFRAME_DEPTH_TYPE_ID, WireframeDepth,
};

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
            Box::new(Blend::new()),
            Box::new(Threshold::new()),
            Box::new(Blur::new()),
            Box::new(MipChain::new()),
            Box::new(Transform::new()),
            Box::new(Sample::new()),
            Box::new(Feedback::new()),
            Box::new(WetDry::new()),
        ]
    }

    #[test]
    fn all_v1_primitives_have_unique_type_ids() {
        let primitives = all_primitives();
        let ids: HashSet<&str> = primitives.iter().map(|p| p.type_id().as_str()).collect();
        assert_eq!(ids.len(), 12, "primitive type IDs must be unique");
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

    /// Hero integration test: assemble the V1 Bloom-as-composite topology
    /// from primitives + boundary nodes, compile it, execute it. Validates
    /// that the trait shape and pool work for a real composite preset.
    ///
    /// Topology:
    ///
    /// ```text
    ///   Source ──→ MipChain ──→ Blur ──→ Blend.overlay ─→ FinalOutput
    ///       │                              ↑
    ///       └─────────────────────────→ Blend.base
    /// ```
    ///
    /// (Threshold is omitted from this test for simplicity — the four-node
    /// shape is enough to exercise fan-out, multi-input, and the boundary.
    /// Real Bloom preset will include Threshold before MipChain.)
    #[test]
    fn bloom_shape_composite_compiles_and_executes() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let mips = g.add_node(Box::new(MipChain::new()));
        let blur = g.add_node(Box::new(Blur::new()));
        let blend = g.add_node(Box::new(Blend::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (mips, "source")).unwrap();
        g.connect((mips, "out"), (blur, "source")).unwrap();
        g.connect((src, "out"), (blend, "base")).unwrap();
        g.connect((blur, "out"), (blend, "overlay")).unwrap();
        g.connect((blend, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 5);

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
            let mut params: AHashMap<&'static str, ParamValue> = AHashMap::default();
            for def in node.parameters() {
                params.insert(def.name, def.default.clone());
            }
            // Pretend every Array input was bound at a large but finite
            // capacity. Same-as-input transforms should resolve against
            // this; producers should ignore it.
            let mut synthetic_inputs: Vec<(&str, u32)> = Vec::new();
            for port in node.inputs() {
                if matches!(port.ty, PortType::Array(_)) {
                    synthetic_inputs.push((port.name, 1024));
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
                if canvas_sized.contains(port.name) {
                    continue;
                }
                let cap = node.array_output_capacity(port.name, &params, &synthetic_inputs);
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
    /// [`ItemKind`](super::super::ports::ItemKind). The kind tag is
    /// what makes wire validation refuse to connect byte-identical
    /// buffers whose conventions don't match — `CurvePoint` (origin-
    /// centered 2D) vs `EdgePair` (two u32 indices) are both 8/4 and
    /// would have connected silently under a pure size/align check.
    /// With the kind tag they don't.
    ///
    /// `ItemKind::Anonymous` is the deliberate opt-out for genuinely
    /// untyped buffers (raw bytes between WGSL escape-hatch nodes,
    /// scratch state). It's allowed for `node.wgsl_compute_*` type-IDs
    /// and the `node.__smoke_test_*` test fixtures — anywhere else it
    /// is a CI failure pointing at a missing
    /// [`KnownItem`](super::super::ports::KnownItem) impl on the item
    /// struct.
    ///
    /// Walks the live [`super::super::PrimitiveRegistry`] so new
    /// primitives are picked up automatically — no central list to
    /// maintain. This is the structural fence against the recurring
    /// coordinate-space contract bug (Tesseract / Duocylinder
    /// spawning in the top-right because a producer used the wrong
    /// convention) — see `crates/manifold-renderer/src/node_graph/ports.rs`
    /// for the `ItemKind` rationale.
    #[test]
    fn every_conventional_array_port_declares_a_kind() {
        use super::super::PrimitiveRegistry;
        use super::super::ports::{ItemKind, PortType};

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        for type_id in registry.known_type_ids() {
            // Carve-outs:
            //   - `node.wgsl_compute_*` — escape-hatch primitives whose
            //     wire shape is whatever the user's WGSL declares.
            //   - `node.__smoke_test_*` — macro-system test fixtures
            //     that exist purely to exercise authoring scaffolding.
            //   - `system.*` — boundary nodes (source, final_output,
            //     generator_input) whose wires are texture/scalar, not
            //     Array, but the carve-out is cheap defensive depth.
            if type_id.starts_with("node.wgsl_compute_")
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
                    && layout.item_kind == ItemKind::Anonymous
                {
                    violations.push(format!(
                        "{type_id}: {kind_label} `{port_name}` is Array<…> \
                         but its ItemKind is Anonymous. Add a `KnownItem` \
                         impl on the item struct (or, if the buffer is \
                         genuinely untyped scratch, extend this test's \
                         carve-out list).",
                    ));
                }
            };

            for port in node.inputs() {
                check_port("input", port.name, &port.ty);
            }
            for port in node.outputs() {
                check_port("output", port.name, &port.ty);
            }
        }
        assert!(
            violations.is_empty(),
            "Array-port ItemKind invariant violations:\n  {}",
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
