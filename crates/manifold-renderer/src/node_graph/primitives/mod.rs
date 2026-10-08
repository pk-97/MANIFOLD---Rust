//! Node implementations for the catalog defined in `docs/NODE_CATALOG.md`.
//!
//! This module hosts both atoms (small generic composable building blocks
//! like Mix, Feedback, Gaussian Blur) and the wrapped legacy effects
//! (Bloom, Watercolor, Halation, etc.) — all as `EffectNode` impls behind
//! flat `node.*` type IDs. The atom/effect split is presentation metadata,
//! not a structural divide.



#[cfg(feature = "gpu-proofs")]
pub use render_scene::rt_proof::{RtProbeObject, RtProbeScene};
#[cfg(feature = "gpu-proofs")]
pub use render_scene::blend_snapshot_proof;
// Standalone staged encoder; the step keeps its existing pressure path.
#[cfg(test)]
mod surface_mesh_freeze_tests;
// Crate-visible so the snapshot builder can key the `(WGSL)` header marker on
// the canonical `TYPE_ID` rather than a duplicated string literal.




pub use atmosphere::AtmosphereNode;
pub use render_mode::RenderModeNode;







pub use array_replicate_polyline_rings::{
    ArrayReplicatePolylineRings, REPLICATE_MAX_RINGS,
};


















pub use bake_equirect_envmap::BakeEquirectEnvmap;





pub use pack_curve_xy::PackCurveXy;




pub use copy_positions::CopyPositions;
pub use wave_shear_mesh::WaveShearMesh;
pub use transform_mesh_patches::TransformMeshPatches;
pub use ordered_recon_mesh::OrderedReconMesh;
pub use mesh_cut_map::{CutMeshBands, CutMeshCells};
pub use remap_mesh_cut::RemapMeshCut;
pub use remap_cut_weights::RemapCutWeights;
pub use morph_mesh::MorphMesh;
pub use normal_wave_mesh::NormalWaveMesh;
pub use mesh_spatial_mask::MeshSpatialMask;
pub use mesh_stagger_envelope::MeshStaggerEnvelope;
pub use analytic_echo_instances::AnalyticEchoInstances;

pub use consecutive_edges::{CONSECUTIVE_EDGES_MAX_CAPACITY, ConsecutiveEdges};


pub use cylinder_wrap_field::CylinderWrapField;

pub use digital_plants_render::DigitalPlantsRender;
pub use displace_mesh::DisplaceMesh;
pub use displace_copies::DisplaceCopies;












pub use fbm_per_instance::FbmPerInstance;





















pub use edges_from_grid_uv::EdgesFromGridUv;
pub use edges_from_mesh::EdgesFromMesh;
pub use edges_from_hypercube::EdgesFromHypercube;

pub use fold_mesh::FoldMesh;
pub use generate_cube_mesh::{CUBE_VERTEX_COUNT, GenerateCubeMesh};
pub use generate_grid_mesh::GenerateGridMesh;
pub use sample_triangle_grid::{SampleTriangleGrid, SAMPLE_TRIANGLE_GRID_CAPACITY};
pub use render_mesh_diagram::RenderMeshDiagram;
pub use generate_grid_uv::{
    GRID_UV_DEFAULT_SIZE, GRID_UV_MAX_SIZE, GenerateGridUv,
};
pub use generate_instance_transforms::{
    GenerateInstanceTransforms, INSTANCE_LAYOUTS,
};

pub use glitch_jitter::GlitchJitter;
pub use gltf_animation_source::GltfAnimationSource;
pub use gltf_mesh_source::GltfMeshSource;
pub use gltf_skeleton_pose::GltfSkeletonPose;
pub use gltf_skinned_mesh_source::GltfSkinnedMeshSource;
pub use gltf_texture_source::GltfTextureSource;





pub use hdri_source::HdriSource;


pub use instance_position_jitter::InstancePositionJitter;
pub use instance_rotation_jitter::InstanceRotationJitter;






pub use ocean_spectrum::OceanSpectrum;
pub use ocean_displace::OceanDisplace;
pub use projected_grid::ProjectedGrid;
pub use cut_out_box::CutOutBox;
pub use camera_sky::CameraSky;

pub use sea_horizon_env::SeaHorizonEnv;

pub use hypercube_vertices::HypercubeVertices;



pub use lerp_instance_fields::LerpInstanceFields;




pub use light::LightNode;

pub use loop_camera::{LOOP_CAMERA_AXIS_LABELS, LoopCamera};





pub use particles_to_copies::ParticlesToCopies;

pub use melt_mesh::MeltMesh;
pub use unlit_material::UnlitMaterial;
pub use pbr_material::PbrMaterial;
pub use cel_material::CelMaterial;




pub use nested_cubes_geometry::{NESTED_CUBES_INSTANCE_COUNT, NestedCubesGeometry};
pub use noise_displace::NoiseDisplace;





pub use plane_mesh::{PLANE_VERTEX_COUNT, GeneratePlaneMesh};



pub use polytope_edges::PolytopeEdges;
pub use polytope_vertices::PolytopeVertices;


pub use project_3d::{PROJECT_3D_MODES, Project3D};
pub use project_4d::Project4D;








pub use reflect_array::ReflectArray;

pub use render_3d_mesh::Render3DMesh;
pub use render_instanced_3d_mesh::RenderInstanced3DMesh;
pub use render_scene::RenderScene;
#[cfg(feature = "fluid-perf-proofs")]
pub use render_scene::water_perf;
pub use render_scene::{arm_rt_capture, disarm_rt_capture, take_rt_captures, RtCaptureSlot};

pub use render_lines::RenderLines;
pub use ripple_mesh::RippleMesh;




pub use rotate_3d::Rotate3D;
pub use rotate_4d::Rotate4D;








pub use scatter_on_mesh::ScatterOnMesh;








pub use simplex_per_instance::SimplexPerInstance;
pub use slice_mesh::SliceMesh;

pub use camera_orbit::{CameraOrbit, DEFAULT_FAR, DEFAULT_NEAR};
pub use free_camera::FreeCamera;
pub use look_at_camera::LookAtCamera;
pub use camera_lens::CameraLens;













pub use torus_wrap_field::TorusWrapField;
pub use triangulate_grid::TriangulateGrid;



pub use transform_3d::Transform3D;
pub use transform_shake::TransformShake;
pub use scene_array::SceneArray;
pub use scene_object::SceneObjectNode;
pub use shatter_mesh::ShatterMesh;




pub use voxelize_mesh::VoxelizeMesh;



#[cfg(test)]
mod tests {
    use manifold_node_engine::primitives::mix::Mix;
use manifold_node_engine::validation::validate;
use manifold_nodes_image::node_graph::primitives::filter::{Blur, Threshold};
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use manifold_node_engine::{exec::effect_node::EffectNode, exec::execution::Executor, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, graph::Graph, parameters::ParamType, parameters::ParamValue, scene::boundary_nodes::Source, exec::execution_plan::compile};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// One boxed instance per registered factory, paired with the id the
    /// factory registered under. Covers every primitive, present and future.
    fn registered_nodes() -> Vec<(&'static str, Box<dyn EffectNode>)> {
        inventory::iter::<manifold_node_engine::persistence::PrimitiveFactory>
            .into_iter()
            .map(|f| (f.type_id, (f.create)()))
            .collect()
    }

    fn assert_no_violations(rule: &str, violations: &[String]) {
        assert!(violations.is_empty(), "{rule}:\n  {}", violations.join("\n  "));
    }

    /// A saved graph finds its node by the registered id, so the node a
    /// factory builds must report that id, or, for a legacy alias, another
    /// registered id. Every id is unique and namespaced, and every atom has
    /// an output. Param defaults match their declared type (an Int
    /// stores its default as a Float and reads back through `as_scalar`, the
    /// path every reader funnels through), and an Enum default indexes a
    /// real option.
    #[test]
    fn every_registered_node_is_well_formed() {
        let nodes = registered_nodes();
        let registered: HashSet<&str> = nodes.iter().map(|(id, _)| *id).collect();
        let mut seen = HashSet::new();
        let mut violations = Vec::new();
        for (type_id, node) in nodes {
            let reported = node.type_id().as_str();
            if reported != type_id && !registered.contains(reported) {
                violations.push(format!("{type_id}: factory builds an unregistered `{reported}`"));
            }
            if !seen.insert(type_id) {
                violations.push(format!("{type_id}: registered twice"));
            }
            if !type_id.starts_with("node.") && !type_id.starts_with("system.") {
                violations.push(format!("{type_id}: id lacks the `node.` or `system.` prefix"));
            }
            // `system.*` sinks end the graph; `node.__*` are test fixtures.
            let is_atom = type_id.starts_with("node.") && !type_id.starts_with("node.__");
            if is_atom && node.outputs().is_empty() {
                violations.push(format!("{type_id}: declares no outputs"));
            }
            for def in node.parameters() {
                let name = &def.name;
                let type_ok = matches!(
                    (def.ty, &def.default),
                    (ParamType::Float | ParamType::Angle | ParamType::Frequency, ParamValue::Float(_))
                        | (ParamType::Int | ParamType::Trigger, ParamValue::Float(_))
                        | (ParamType::Bool, ParamValue::Bool(_))
                        | (ParamType::Vec2, ParamValue::Vec2(_))
                        | (ParamType::Vec3, ParamValue::Vec3(_))
                        | (ParamType::Vec4, ParamValue::Vec4(_))
                        | (ParamType::Color, ParamValue::Color(_))
                        | (ParamType::Enum, ParamValue::Enum(_))
                        // Tables and Strings can't live in a const default, so
                        // a Float placeholder stands in until the preset
                        // overrides it.
                        | (ParamType::String, ParamValue::Float(_) | ParamValue::String(_))
                        | (ParamType::Table, ParamValue::Float(_) | ParamValue::Table(_))
                );
                if !type_ok {
                    violations.push(format!(
                        "{type_id} param `{name}`: default {:?} does not match declared type {:?}",
                        def.default, def.ty
                    ));
                    continue;
                }
                if def.ty == ParamType::Int
                    && let ParamValue::Float(stored) = def.default
                    && def.default.as_scalar() != Some(stored)
                {
                    violations.push(format!("{type_id} param `{name}`: Int default does not read back through as_scalar"));
                }
                if let ParamValue::Enum(idx) = def.default
                    && idx as usize >= def.enum_values.len()
                {
                    violations.push(format!(
                        "{type_id} param `{name}`: enum default {idx} out of range for {} options",
                        def.enum_values.len()
                    ));
                }
            }
        }
        assert_no_violations("registered-node shape violations", &violations);
    }

    /// Shadow inputs that are required, so their same-named param can never
    /// act as the unwired fallback. Known and pinned here; new ones fail.
    const REQUIRED_SHADOW_INPUTS: &[(&str, &str)] = &[
        ("node.math", "a"),
        ("node.scale_offset_value", "a"),
        ("node.switch_array", "selector"),
        ("node.switch_texture", "selector"),
        ("node.switch_value", "selector"),
    ];

    /// Angles that are winding amounts rather than orientations. A range on
    /// these clamps a wired or typed value at one turn, the saw-rotation-wrap
    /// class, so they must stay unbounded.
    const UNBOUNDED_WINDING_ANGLES: &[(&str, &str)] = &[
        ("node.bend_mesh", "angle"),
        ("node.revolve_curve", "sweep"),
        ("node.twist_mesh", "angle"),
    ];

    /// Port-shadow convention: an input named like a param lets a wire
    /// override that param. The wire is optional, since the param is the
    /// unwired fallback, and it carries the param's scalar type. Bool, Enum,
    /// Int and Trigger params travel as a plain f32 wire. Table and String
    /// params cannot be driven by a scalar wire, so they are never shadowed.
    #[test]
    fn port_shadow_inputs_are_optional_and_typed_like_their_param() {
        use manifold_node_engine::ports::{PortType, ScalarType};
        let mut violations = Vec::new();
        let mut unbounded_seen = 0;
        for (type_id, node) in registered_nodes() {
            let params = node.parameters();
            for port in node.inputs() {
                let Some(def) = params.iter().find(|d| d.name == port.name) else {
                    continue;
                };
                let name: &str = &port.name;
                if port.required && !REQUIRED_SHADOW_INPUTS.contains(&(type_id, name)) {
                    violations.push(format!("{type_id}: shadow input `{name}` is required"));
                }
                let expected = match def.ty {
                    ParamType::Float
                    | ParamType::Angle
                    | ParamType::Frequency
                    | ParamType::Int
                    | ParamType::Bool
                    | ParamType::Enum
                    | ParamType::Trigger => Some(ScalarType::F32),
                    ParamType::Vec2 => Some(ScalarType::Vec2),
                    ParamType::Vec3 => Some(ScalarType::Vec3),
                    ParamType::Vec4 => Some(ScalarType::Vec4),
                    ParamType::Color => Some(ScalarType::Color),
                    ParamType::Table | ParamType::String => None,
                };
                match expected {
                    None => violations.push(format!(
                        "{type_id}: {:?} param `{name}` must not be port-shadowed",
                        def.ty
                    )),
                    Some(scalar) if port.ty != PortType::Scalar(scalar) => violations.push(format!(
                        "{type_id}: shadow input `{name}` is {:?}, param wants {scalar:?}",
                        port.ty
                    )),
                    Some(_) => {}
                }
            }
            for def in params {
                let name: &str = &def.name;
                if !UNBOUNDED_WINDING_ANGLES.contains(&(type_id, name)) {
                    continue;
                }
                unbounded_seen += 1;
                if def.range.is_some() {
                    violations.push(format!("{type_id}: winding angle `{name}` must be unbounded"));
                }
            }
        }
        if unbounded_seen != UNBOUNDED_WINDING_ANGLES.len() {
            violations.push("an UNBOUNDED_WINDING_ANGLES entry names a param that no longer exists".into());
        }
        assert_no_violations("port-shadow violations", &violations);
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
            Err(manifold_node_engine::validation::GraphError::RequiredInputUnwired { .. })
        ));
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
    /// `node.__smoke_test_*` fixtures. A `Channels[permissive]` port has
    /// no fixed signature by design; the Permissive allow-list test in
    /// `validation.rs` is its gate. Anywhere else it's a CI
    /// failure pointing at a missing `KnownItem::SPECS` or a missing
    /// inline `Channels[…]` declaration.
    ///
    /// Walks the live [`super::super::PrimitiveRegistry`] so new
    /// primitives are picked up automatically.
    #[test]
    fn every_conventional_array_port_declares_a_channels_signature() {
        use manifold_node_engine::persistence::PrimitiveRegistry;
        use manifold_node_engine::ports::PortType;

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
                    && layout.match_mode != manifold_node_engine::ports::MatchMode::Permissive
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




