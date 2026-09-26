//! P7a stock scene-modifier catalog and composition acceptance.
//!
//! Every bundled scene-modifier file is attached through the authoring path,
//! rendered through `PresetRuntime`, and checked for both real RT dispatch
//! and a numerical witness derived from the current frame's GPU mesh bytes.
//! The witness is intentionally independent of beauty pixels: it computes a
//! centroid ray and Möller–Trumbore hit from the final `MeshVertex` output.

use std::fs;
use std::path::Path;

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::preset_def::PresetKind;
use manifold_core::scene_modifier_preset::SceneTargetSelection;
use manifold_gpu::GpuBuffer;
use manifold_gpu::GpuTextureFormat;
use manifold_gpu::raytrace::{DebugRayQueryHit, DebugRayQueryRay};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::generators::mesh_common::InstanceTransform;
use manifold_renderer::generators::mesh_common::MeshVertex;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::loaded_scene_modifier_presets_from_bundled;
use manifold_renderer::node_graph::primitives::{RtProbeObject, RtProbeScene};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const EXPECTED_STOCK_IDS: &[&str] = &[
    "ElasticSculpture",
    "MaskedPeel",
    "MathView",
    "OrderedRecon",
    "OrderedReconHit",
    "RenderMode",
    "SceneFog",
    "SceneLoop",
    "SpatialEchoes",
    "SurfacePeel",
    "SurfacePeelHit",
    "SurfaceWaves",
    "VortexFragments",
    "WavesEchoes",
];

fn recipe_json(id: &str) -> &'static str {
    match id {
        "ElasticSculpture" => {
            include_str!("../../assets/scene-modifier-presets/ElasticSculpture.json")
        }
        "MaskedPeel" => include_str!("../../assets/scene-modifier-presets/MaskedPeel.json"),
        "MathView" => include_str!("../../assets/scene-modifier-presets/MathView.json"),
        "OrderedRecon" => include_str!("../../assets/scene-modifier-presets/OrderedRecon.json"),
        "OrderedReconHit" => {
            include_str!("../../assets/scene-modifier-presets/OrderedReconHit.json")
        }
        "RenderMode" => include_str!("../../assets/scene-modifier-presets/RenderMode.json"),
        "SceneFog" => include_str!("../../assets/scene-modifier-presets/SceneFog.json"),
        "SceneLoop" => include_str!("../../assets/scene-modifier-presets/SceneLoop.json"),
        "SpatialEchoes" => include_str!("../../assets/scene-modifier-presets/SpatialEchoes.json"),
        "SurfacePeel" => include_str!("../../assets/scene-modifier-presets/SurfacePeel.json"),
        "SurfacePeelHit" => include_str!("../../assets/scene-modifier-presets/SurfacePeelHit.json"),
        "SurfaceWaves" => include_str!("../../assets/scene-modifier-presets/SurfaceWaves.json"),
        "VortexFragments" => {
            include_str!("../../assets/scene-modifier-presets/VortexFragments.json")
        }
        "WavesEchoes" => include_str!("../../assets/scene-modifier-presets/WavesEchoes.json"),
        other => panic!("unknown scene-modifier acceptance fixture {other}"),
    }
}

fn discovered_stock_ids() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/scene-modifier-presets");
    let mut ids: Vec<String> = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read catalog entry: {error}"))
                .path()
        })
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter_map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)
        })
        .collect();
    ids.sort();
    ids
}

fn catalog_host() -> EffectGraphDef {
    let mut owner: EffectGraphDef = serde_json::from_str(include_str!(
        "../fixtures/scene-modifiers/nested_multimaterial_v2.json"
    ))
    .expect("catalog host fixture must parse");
    owner.version = 3;
    let scene = owner
        .nodes
        .iter_mut()
        .find(|node| node.type_id == "node.render_scene")
        .expect("catalog host has render_scene");
    scene.params.insert(
        "rt_enabled".into(),
        SerializedParamValue::Bool { value: true },
    );
    let scene_id = scene.id;
    owner.nodes.push(
        serde_json::from_value(serde_json::json!({
            "id": 40,
            "nodeId": "catalog_environment",
            "typeId": "node.bake_environment",
            "params": {
                "width": {"type": "Int", "value": 64},
                "height": {"type": "Int", "value": 32},
                "uniform": {"type": "Bool", "value": true}
            }
        }))
        .expect("catalog environment node must deserialize"),
    );
    owner
        .wires
        .push(manifold_core::effect_graph_def::EffectGraphWire {
            from_node: 40,
            from_port: "envmap".into(),
            to_node: scene_id,
            to_port: "envmap".into(),
        });
    owner
}

fn render_scene_ref(owner: &EffectGraphDef) -> manifold_core::scene_modifier_preset::SceneNodeRef {
    let scene = owner
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .expect("catalog host has render_scene");
    manifold_core::scene_modifier_preset::SceneNodeRef {
        scope: Vec::new(),
        node: scene.node_id.clone(),
    }
}

fn attach(mut owner: EffectGraphDef, ids: &[&str]) -> EffectGraphDef {
    for (index, id) in ids.iter().enumerate() {
        // Generated cubes use the same saved source-reference snapshots as
        // the existing production combo proof. Fresh authoring capture is
        // deliberately restricted to imported glTF sources.
        let recipe = serde_json::from_str(recipe_json(id)).expect("stock recipe must parse");
        let graph = manifold_renderer::node_graph::scene_modifier_authoring::initialize_scene_modifier_graph(&owner, &recipe).unwrap();
        let mut instance = manifold_core::scene_modifier_preset::SceneModifierInstanceDef {
            id: format!("catalog_{index}_{id}").into(),
            scene: render_scene_ref(&owner),
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(graph),
        };
        instance.mesh_frames =
            match manifold_renderer::node_graph::scene_modifier_expand::resolve_modifier_mesh_frames(
                &owner, &instance,
            ) {
                Ok(frames) => frames,
                Err(error) => {
                    assert!(
                        error.to_string().contains(
                            "fresh coordinate capture requires a direct static glTF mesh source"
                        ),
                        "{id}: {error}"
                    );
                    super::rt_dynamic_current_frame::modifier_combo_scene().scene_modifiers[0]
                        .mesh_frames
                        .clone()
                }
            };
        owner = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &owner,
            owner.scene_modifiers.len(),
            instance,
        )
        .unwrap_or_else(|error| panic!("{id} must reconcile catalog host: {error}"))
        .graph;
    }
    owner
}

#[derive(Clone, Copy)]
struct Triangle {
    p: [[f32; 3]; 3],
}

#[derive(Clone, Copy)]
struct ProbeWitness {
    triangle: Triangle,
}

const DETERMINISTIC_RAY_COUNT: usize = 64;

#[derive(Clone, Copy)]
struct ProbeHitExpectation {
    ray: DebugRayQueryRay,
    object_id: u32,
    primitive_id: u32,
    instance_id: u32,
    oracle: (f32, f32, f32),
}

/// CPU-visible copies of the exact buffers handed to the resident RT scene.
/// Metal mesh buffers are commonly private, so the oracle must use a blit
/// readback rather than silently skipping an unmapped source buffer.
struct ProbeObjectSnapshot {
    vertices: GpuBuffer,
    indices: Option<GpuBuffer>,
    vertex_stride: u32,
    vertex_offset: u32,
    triangle_count: u32,
    transform: [[f32; 4]; 4],
    instances: Option<GpuBuffer>,
    instance_slots: u32,
    weights: Option<GpuBuffer>,
    gain: f32,
}

fn transform_model(model: [[f32; 4]; 4], point: [f32; 3]) -> [f32; 3] {
    [
        model[0][0] * point[0] + model[1][0] * point[1] + model[2][0] * point[2] + model[3][0],
        model[0][1] * point[0] + model[1][1] * point[1] + model[2][1] * point[2] + model[3][1],
        model[0][2] * point[0] + model[1][2] * point[1] + model[2][2] * point[2] + model[3][2],
    ]
}

fn transform_instance(point: [f32; 3], instance: InstanceTransform) -> [f32; 3] {
    let (sx, cx) = instance.rot_pad[0].sin_cos();
    let (sy, cy) = instance.rot_pad[1].sin_cos();
    let (sz, cz) = instance.rot_pad[2].sin_cos();
    // This is the column-major euler_xyz() used by render_scene.wgsl.
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
    let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
    let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let rotation = mat3_mul(mat3_mul(rz, ry), rx);
    let marker = (instance.rot_pad[3] + 0.5) as u32;
    let sign = [
        if marker == 1 { -1.0 } else { 1.0 },
        if marker == 2 { -1.0 } else { 1.0 },
        if marker == 3 { -1.0 } else { 1.0 },
    ];
    let scaled = [
        point[0] * sign[0] * instance.pos_scale[3],
        point[1] * sign[1] * instance.pos_scale[3],
        point[2] * sign[2] * instance.pos_scale[3],
    ];
    let rotated = mat3_vec(rotation, scaled);
    [
        rotated[0] + instance.pos_scale[0],
        rotated[1] + instance.pos_scale[1],
        rotated[2] + instance.pos_scale[2],
    ]
}

fn mat3_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    for column in 0..3 {
        result[column] = mat3_vec(a, b[column]);
    }
    result
}

fn mat3_vec(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * vector[0] + matrix[1][0] * vector[1] + matrix[2][0] * vector[2],
        matrix[0][1] * vector[0] + matrix[1][1] * vector[1] + matrix[2][1] * vector[2],
        matrix[0][2] * vector[0] + matrix[1][2] * vector[1] + matrix[2][2] * vector[2],
    ]
}

fn read_vertex(object: &ProbeObjectSnapshot, index: u32) -> Option<MeshVertex> {
    let ptr = object.vertices.mapped_ptr()?;
    let offset = object.vertex_offset as usize + index as usize * object.vertex_stride as usize;
    (offset + std::mem::size_of::<MeshVertex>() <= object.vertices.size as usize)
        .then(|| unsafe { (ptr.add(offset) as *const MeshVertex).read_unaligned() })
}

fn read_index(object: &ProbeObjectSnapshot, index: u32) -> Option<u32> {
    let buffer = object.indices.as_ref()?;
    let ptr = buffer.mapped_ptr()?;
    let offset = index as usize * std::mem::size_of::<u32>();
    (offset + 4 <= buffer.size as usize)
        .then(|| unsafe { (ptr.add(offset) as *const u32).read_unaligned() })
}

fn snapshot_scene(
    device: &manifold_gpu::GpuDevice,
    encoder: &mut manifold_gpu::GpuEncoder,
    scene: &RtProbeScene,
) -> Vec<ProbeObjectSnapshot> {
    scene
        .objects
        .iter()
        .map(|object: &RtProbeObject| {
            let mut copy = |source: &GpuBuffer| {
                let destination = device.create_buffer_shared(source.size.max(16));
                if source.size != 0 {
                    encoder.copy_buffer_to_buffer(source, &destination, source.size);
                }
                destination
            };
            ProbeObjectSnapshot {
                vertices: copy(&object.vertices),
                indices: object.indices.as_ref().map(&mut copy),
                vertex_stride: object.vertex_stride,
                vertex_offset: object.vertex_offset,
                triangle_count: object.triangle_count,
                transform: object.transform,
                instances: object.instances.as_ref().map(&mut copy),
                instance_slots: object.instance_slots,
                weights: object.weights.as_ref().map(&mut copy),
                gain: object.gain,
            }
        })
        .collect()
}

fn slot_base(scene: &[ProbeObjectSnapshot], object: usize) -> u32 {
    scene[..object]
        .iter()
        .map(|object| {
            if object.instances.is_some() {
                object.instance_slots.max(1)
            } else {
                1
            }
        })
        .sum()
}

fn probe_witness_first(scene: &[ProbeObjectSnapshot]) -> ProbeWitness {
    for object in scene {
        let slots = if object.instances.is_some() {
            object.instance_slots.max(1)
        } else {
            1
        };
        for instance in 0..slots {
            for primitive_id in 0..object.triangle_count {
                let Some(triangle) = candidate_triangle(object, instance, primitive_id) else {
                    continue;
                };
                let normal = cross(
                    sub(triangle.p[1], triangle.p[0]),
                    sub(triangle.p[2], triangle.p[0]),
                );
                let length = dot(normal, normal).sqrt();
                if !length.is_finite() || length < 1e-6 {
                    continue;
                }
                return ProbeWitness { triangle };
            }
        }
    }
    panic!("production RT probe has no fully covered nondegenerate triangle");
}

fn candidate_triangle(
    object: &ProbeObjectSnapshot,
    slot: u32,
    primitive_id: u32,
) -> Option<Triangle> {
    if primitive_id >= object.triangle_count || !object.gain.is_finite() || object.gain <= 0.0 {
        return None;
    }
    let instance = if let Some(buffer) = object.instances.as_ref() {
        let ptr = buffer.mapped_ptr()?;
        let offset = slot as usize * std::mem::size_of::<InstanceTransform>();
        if offset + std::mem::size_of::<InstanceTransform>() > buffer.size as usize {
            return None;
        }
        let value = unsafe { (ptr.add(offset) as *const InstanceTransform).read_unaligned() };
        if !value.pos_scale[3].is_finite() || value.pos_scale[3] == 0.0 {
            return None;
        }
        Some(value)
    } else {
        None
    };
    let base = primitive_id.checked_mul(3)?;
    let indices = if object.indices.is_some() {
        [
            read_index(object, base)?,
            read_index(object, base + 1)?,
            read_index(object, base + 2)?,
        ]
    } else {
        [base, base + 1, base + 2]
    };
    if let Some(weights) = object.weights.as_ref() {
        let ptr = weights.mapped_ptr()?;
        let count = weights.size as usize / std::mem::size_of::<f32>();
        let mut level = 0.0;
        for index in indices {
            if index as usize >= count {
                return None;
            }
            level += unsafe { (ptr.add(index as usize * 4) as *const f32).read_unaligned() } / 3.0;
        }
        if !(level * object.gain).is_finite() || level * object.gain < 1.0 {
            return None;
        }
    } else if object.gain < 1.0 {
        return None;
    }
    let vertices = [
        read_vertex(object, indices[0])?,
        read_vertex(object, indices[1])?,
        read_vertex(object, indices[2])?,
    ];
    let mut positions = [
        vertices[0].position,
        vertices[1].position,
        vertices[2].position,
    ];
    for position in &mut positions {
        if let Some(instance) = instance {
            *position = transform_instance(*position, instance);
        }
        *position = transform_model(object.transform, *position);
    }
    Some(Triangle { p: positions })
}

#[allow(clippy::type_complexity)]
fn nearest_hit(
    scene: &[ProbeObjectSnapshot],
    ray: DebugRayQueryRay,
) -> Option<(Triangle, u32, u32, u32, (f32, f32, f32))> {
    let mut nearest: Option<(f32, f32, f32, Triangle, u32, u32, u32)> = None;
    for (object_id, object) in scene.iter().enumerate() {
        let slots = object
            .instances
            .as_ref()
            .map_or(1, |_| object.instance_slots.max(1));
        for instance_id in 0..slots {
            for primitive_id in 0..object.triangle_count {
                let Some(triangle) = candidate_triangle(object, instance_id, primitive_id) else {
                    continue;
                };
                let Some((distance, u, v)) = moller_trumbore(ray.origin, ray.direction, triangle)
                else {
                    continue;
                };
                if distance < ray.min_distance || distance > ray.max_distance {
                    continue;
                }
                if nearest
                    .as_ref()
                    .is_none_or(|candidate| distance < candidate.0)
                {
                    nearest = Some((
                        distance,
                        u,
                        v,
                        triangle,
                        object_id as u32,
                        primitive_id,
                        slot_base(scene, object_id) + instance_id,
                    ));
                }
            }
        }
    }
    nearest.map(
        |(_, u, v, triangle, object_id, primitive_id, instance_id)| {
            let distance = moller_trumbore(ray.origin, ray.direction, triangle)
                .expect("nearest candidate must intersect")
                .0;
            (
                triangle,
                object_id,
                primitive_id,
                instance_id,
                (distance, u, v),
            )
        },
    )
}

fn deterministic_probe_expectations(
    scene: &[ProbeObjectSnapshot],
    triangle: Triangle,
) -> Vec<ProbeHitExpectation> {
    let normal = cross(
        sub(triangle.p[1], triangle.p[0]),
        sub(triangle.p[2], triangle.p[0]),
    );
    let length = dot(normal, normal).sqrt();
    assert!(
        length.is_finite() && length >= 1e-6,
        "probe triangle must be nondegenerate"
    );
    let unit = normal.map(|value| value / length);
    let mut expectations = Vec::with_capacity(DETERMINISTIC_RAY_COUNT);
    // Query the nearest physical hit including edge candidates first, then
    // select64rays whose actual nearest hit is interior. Excluding edge
    // candidates from intersection itself would invent a farther CPU hit.
    'samples: for a in 2..31 {
        for b in 2..(31 - a) {
            let u = a as f32 / 32.0;
            let v = b as f32 / 32.0;
            let point = [
                triangle.p[0][0] * (1.0 - u - v) + triangle.p[1][0] * u + triangle.p[2][0] * v,
                triangle.p[0][1] * (1.0 - u - v) + triangle.p[1][1] * u + triangle.p[2][1] * v,
                triangle.p[0][2] * (1.0 - u - v) + triangle.p[1][2] * u + triangle.p[2][2] * v,
            ];
            let ray = DebugRayQueryRay {
                origin: [
                    point[0] + unit[0] * 0.01,
                    point[1] + unit[1] * 0.01,
                    point[2] + unit[2] * 0.01,
                ],
                direction: unit.map(|value| -value),
                min_distance: 0.0,
                max_distance: 0.02,
            };
            let (_, object_id, primitive_id, instance_id, oracle) = nearest_hit(scene, ray)
                .unwrap_or_else(|| panic!("probe lattice ray missed final geometry at ({u}, {v})"));
            if oracle.1 < 0.05 || oracle.2 < 0.05 || oracle.1 + oracle.2 > 0.95 {
                continue;
            }
            expectations.push(ProbeHitExpectation {
                ray,
                object_id,
                primitive_id,
                instance_id,
                oracle,
            });
            if expectations.len() == DETERMINISTIC_RAY_COUNT {
                break 'samples;
            }
        }
    }
    assert_eq!(expectations.len(), DETERMINISTIC_RAY_COUNT);
    expectations
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn moller_trumbore(
    origin: [f32; 3],
    direction: [f32; 3],
    triangle: Triangle,
) -> Option<(f32, f32, f32)> {
    let e1 = sub(triangle.p[1], triangle.p[0]);
    let e2 = sub(triangle.p[2], triangle.p[0]);
    let p = cross(direction, e2);
    let determinant = dot(e1, p);
    if determinant.abs() < 1e-8 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let tvec = sub(origin, triangle.p[0]);
    let u = dot(tvec, p) * inverse;
    let q = cross(tvec, e1);
    let v = dot(direction, q) * inverse;
    let t = dot(e2, q) * inverse;
    (t >= 0.0 && u >= 0.0 && v >= 0.0 && u + v <= 1.0).then_some((t, u, v))
}

fn render_and_witness(owner: EffectGraphDef, label: &str, expected_frames: usize) {
    render_and_witness_controlled(owner, label, expected_frames, None, None);
}

fn render_and_witness_controlled(
    owner: EffectGraphDef,
    label: &str,
    expected_frames: usize,
    control: Option<(&str, &str, &[f32])>,
    writer_offsets: Option<&[f32]>,
) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let metadata = owner.preset_metadata.as_ref();
    let mut manifest = manifold_core::params::ParamManifest::from_params(
        metadata
            .map(|metadata| {
                metadata
                    .params
                    .iter()
                    .cloned()
                    .map(manifold_core::params::Param::bundled)
                    .collect()
            })
            .unwrap_or_default(),
    );
    let control_id = control.map(|(_, param, _)| {
        metadata.unwrap().bindings.iter().find(|binding| matches!(&binding.target,
            manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. } if param_id.as_str() == param
        )).unwrap_or_else(|| panic!("{label}: no production modifier binding for {param}"))
            .id.clone()
    });
    let mut runtime = PresetRuntime::from_def_with_device(
        owner,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|error| panic!("{label} runtime must build: {error}"));
    let target = h.make_target(label);
    for frame in 0..expected_frames {
        if let Some((_, _, values)) = control {
            assert_eq!(
                values.len(),
                expected_frames,
                "{label} control sequence length"
            );
            let param = manifest.get_mut(control_id.as_ref().unwrap()).unwrap();
            assert!((param.spec.min..=param.spec.max).contains(&values[frame]));
            param.value = values[frame];
        }
        if let Some(writer) = runtime
            .graph
            .instance_by_node_id(&manifold_core::NodeId::new("custom_writer"))
        {
            runtime
                .graph
                .set_param(
                    writer,
                    "offset",
                    manifold_renderer::node_graph::ParamValue::Float(writer_offsets.map_or(
                        frame as f32 * 0.25,
                        |values| {
                            assert_eq!(
                                values.len(),
                                expected_frames,
                                "{label} writer sequence length"
                            );
                            values[frame]
                        },
                    )),
                )
                .unwrap();
        }
        runtime.set_dump_all(true);
        let context = PresetContext {
            time: frame as f64 / 24.0,
            beat: frame as f64 / 12.0,
            dt: if frame == 0 { 0.0 } else { 1.0 / 24.0 },
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut status = None;
        let mut snapshot = None;
        let mut expectations = None;
        let mut query_buffer: Option<GpuBuffer> = None;
        let captures = harness::capture_rt_channels(|| {
            let mut encoder = h.device.create_encoder(label);
            {
                let mut gpu = RendererGpuEncoder::new(&mut encoder, &h.device);
                gpu.capture_rt_geometry = true;
                runtime.render(&mut gpu, &target.texture, &context, &manifest);
                let scene = runtime
                    .rt_probe_scene()
                    .unwrap_or_else(|| panic!("{label} frame {frame} must capture RT geometry"));
                snapshot = Some(snapshot_scene(&h.device, gpu.native_enc, scene));
                status = Some(gpu.frame_status());
            }
            encoder.commit_and_wait_completed();
            let cpu_scene = snapshot.as_ref().expect("RT geometry snapshot committed");
            let selected = probe_witness_first(cpu_scene);
            let sample_expectations =
                deterministic_probe_expectations(cpu_scene, selected.triangle);
            let rays: Vec<_> = sample_expectations
                .iter()
                .map(|sample| sample.ray)
                .collect();
            expectations = Some(sample_expectations);

            // Query the same resident acceleration structure after its render
            // and geometry-copy command buffers have completed. This keeps the
            // CPU oracle independent of the production query while ensuring
            // the readback bytes are from this frame's final geometry.
            let mut query_encoder = h.device.create_encoder("catalog-rt-query");
            query_buffer = runtime.rt_probe_rays(&h.device, &mut query_encoder, &rays);
            query_encoder.commit_and_wait_completed();
        });
        assert_eq!(
            status,
            Some(FrameRenderStatus::Complete),
            "{label} frame {frame}"
        );
        assert!(
            !captures.is_empty(),
            "{label} frame {frame} must dispatch RT"
        );
        let expectations = expectations.expect("RT witnesses must be recorded");
        assert_eq!(expectations.len(), DETERMINISTIC_RAY_COUNT);
        let hit_buffer = query_buffer.expect("RT probe query must return a hit buffer");
        let hit_ptr = hit_buffer
            .mapped_ptr()
            .expect("RT probe hit buffer must be CPU-mapped");
        let mut coincident_hits = 0;
        for (sample_index, expected) in expectations.iter().enumerate() {
            let hit = unsafe {
                hit_ptr
                    .add(sample_index * std::mem::size_of::<DebugRayQueryHit>())
                    .cast::<DebugRayQueryHit>()
                    .read_unaligned()
            };
            assert_eq!(
                hit.hit, 1,
                "{label} frame {frame} ray {sample_index} missed"
            );
            let tolerance = 1e-4f32.max(1e-4 * expected.oracle.0.abs());
            assert!(
                (hit.distance - expected.oracle.0).abs() <= tolerance,
                "{label} frame {frame} ray {sample_index} distance {} vs CPU {} (tol {tolerance})",
                hit.distance,
                expected.oracle.0
            );
            let oracle = if (hit.object_id, hit.instance_id, hit.primitive_id)
                == (
                    expected.object_id,
                    expected.instance_id,
                    expected.primitive_id,
                ) {
                expected.oracle
            } else {
                // Cut recipes can contain coincident triangles. Metal need
                // not choose the CPU iteration order among equal-distance
                // hits. Verify the exact returned triangle and nearest
                // distance before comparing its own barycentrics.
                let scene = snapshot.as_ref().unwrap();
                let object = scene
                    .get(hit.object_id as usize)
                    .expect("RT object ID in bounds");
                let instance = hit
                    .instance_id
                    .checked_sub(slot_base(scene, hit.object_id as usize))
                    .expect("RT instance ID belongs to object");
                assert!(
                    instance
                        < object
                            .instances
                            .as_ref()
                            .map_or(1, |_| object.instance_slots)
                );
                let triangle = candidate_triangle(object, instance, hit.primitive_id)
                    .expect("RT primitive ID names a valid covered triangle");
                let actual = moller_trumbore(expected.ray.origin, expected.ray.direction, triangle)
                    .expect("returned RT triangle intersects the exact query ray");
                assert!(
                    (actual.0 - expected.oracle.0).abs() <= 1e-6,
                    "{label} ray {sample_index}: non-nearest IDs GPU({},{},{}) CPU({},{},{}), distances {} vs {}",
                    hit.object_id,
                    hit.instance_id,
                    hit.primitive_id,
                    expected.object_id,
                    expected.instance_id,
                    expected.primitive_id,
                    actual.0,
                    expected.oracle.0
                );
                coincident_hits += 1;
                actual
            };
            assert!(
                (hit.bary[0] - oracle.1).abs() <= 2e-4,
                "{label} frame {frame} ray {sample_index} bary.u {} vs CPU {}",
                hit.bary[0],
                oracle.1
            );
            assert!(
                (hit.bary[1] - oracle.2).abs() <= 2e-4,
                "{label} frame {frame} ray {sample_index} bary.v {} vs CPU {}",
                hit.bary[1],
                oracle.2
            );
        }
        println!(
            "{label} frame {frame}: {DETERMINISTIC_RAY_COUNT} production RT hits matched CPU ({coincident_hits} coincident triangles)"
        );
    }
}

#[test]
fn rt_dynamic_catalog_all_stock_and_compositions() {
    let mut expected: Vec<String> = EXPECTED_STOCK_IDS.iter().map(|id| (*id).into()).collect();
    expected.sort();
    assert_eq!(
        discovered_stock_ids(),
        expected,
        "every new stock recipe needs an acceptance fixture"
    );

    let mut catalog_ids: Vec<String> =
        manifold_renderer::node_graph::bundled_preset_type_ids(PresetKind::SceneModifier)
            .map(|id| id.as_str().to_owned())
            .collect();
    catalog_ids.sort();
    assert_eq!(
        catalog_ids, expected,
        "runtime catalog must discover every stock recipe"
    );
    let metadata = loaded_scene_modifier_presets_from_bundled();
    for id in EXPECTED_STOCK_IDS {
        assert!(
            metadata.iter().any(|entry| entry.id.as_str() == *id),
            "missing metadata for {id}"
        );
    }

    for id in EXPECTED_STOCK_IDS {
        render_and_witness(attach(catalog_host(), &[id]), id, 3);
    }

    for (label, ids) in [
        ("waves-then-cuts", &["SurfaceWaves", "OrderedRecon"][..]),
        ("cuts-then-waves", &["OrderedRecon", "SurfaceWaves"][..]),
        ("waves-then-echoes", &["SurfaceWaves", "SpatialEchoes"][..]),
        ("two-waves", &["SurfaceWaves", "SurfaceWaves"][..]),
        (
            "cut-waves-echoes",
            &["OrderedRecon", "SurfaceWaves", "SpatialEchoes"][..],
        ),
    ] {
        let owner = attach(catalog_host(), ids);
        render_and_witness(owner.clone(), label, 2);
        let mut reordered = owner;
        reordered.scene_modifiers.reverse();
        render_and_witness(reordered, &format!("{label}-reordered"), 1);
    }

    let saved = serde_json::to_string(&attach(catalog_host(), &["SurfaceWaves", "OrderedRecon"]))
        .expect("catalog composition must serialize");
    let reloaded: EffectGraphDef =
        serde_json::from_str(&saved).expect("catalog composition must reload");
    render_and_witness(reloaded, "catalog-save-reload", 2);
}

#[test]
fn rt_dynamic_catalog_authored_unknown_mesh_writer() {
    render_and_witness(
        authored_unknown_writer_graph(),
        "authored-unknown-writer",
        3,
    );
}

fn authored_unknown_writer_graph() -> EffectGraphDef {
    let mut graph: serde_json::Value =
        serde_json::from_str(super::rt_dynamic_current_frame::scene_json()).unwrap();
    let shader = r#"
struct MeshVertex {
    position:vec3<f32>,
    // @channel_skip
    _pad0:f32,
    normal:vec3<f32>,
    // @channel_skip
    _pad1:f32,
    uv:vec2<f32>,
    uv1:vec2<f32>,
    tangent:vec4<f32>,
    color:vec4<f32>
};
struct U { offset:f32 };
@group(0) @binding(0) var<uniform> u:U;
@group(0) @binding(1) var<storage,read> src:array<MeshVertex>;
@group(0) @binding(2) var<storage,read_write> vertices:array<MeshVertex>;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x >= arrayLength(&src) || id.x >= arrayLength(&vertices) { return; }
    var v = src[id.x];
    v.position.x += u.offset;
    vertices[id.x] = v;
}
"#;
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id":30,"nodeId":"custom_writer","typeId":"node.wgsl_compute","wgslSource":shader,
            "params":{"offset":{"type":"Float","value":0.0}}
        }));
    for wire in graph["wires"].as_array_mut().unwrap() {
        if wire["fromNode"] == 2 && wire["toNode"] == 4 {
            wire["toNode"] = 30.into();
            wire["toPort"] = "src".into();
        }
    }
    graph["wires"].as_array_mut().unwrap().push(
        serde_json::json!({"fromNode":30,"fromPort":"vertices","toNode":4,"toPort":"vertices"}),
    );
    graph["wires"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"fromNode":2,"fromPort":"out","toNode":30,"toPort":"vertices"}));
    serde_json::from_value(graph).unwrap()
}

#[test]
fn rt_dynamic_catalog_endpoint_pause_backward_seek_controls() {
    // Each sequence includes the documented lower/upper endpoints, a repeated
    // paused value, and a backward seek. The same resident-AS numerical
    // witness runs after every control update, so a stale deformation or
    // instance table cannot satisfy the matrix through dispatch alone.
    const WAVES_PHASE: &[f32] = &[0.0, 1.0, 1.0, 0.25, 0.0];
    const CUT_PROGRESS: &[f32] = &[0.0, 1.0, 1.0, 0.25, 0.0];
    const ECHO_COUNT: &[f32] = &[1.0, 8.0, 8.0, 3.0, 1.0];

    render_and_witness_controlled(
        attach(catalog_host(), &["SurfaceWaves"]),
        "surface-waves-endpoints",
        WAVES_PHASE.len(),
        Some(("wave", "phase", WAVES_PHASE)),
        None,
    );
    render_and_witness_controlled(
        attach(catalog_host(), &["OrderedRecon"]),
        "ordered-recon-endpoints",
        CUT_PROGRESS.len(),
        Some(("recon", "progress", CUT_PROGRESS)),
        None,
    );
    render_and_witness_controlled(
        attach(catalog_host(), &["SpatialEchoes"]),
        "spatial-echoes-instance-endpoints",
        ECHO_COUNT.len(),
        Some(("analytic_echo", "count", ECHO_COUNT)),
        None,
    );

    const WRITER_OFFSET: &[f32] = &[-0.5, 0.5, 0.5, 0.125, -0.5];
    render_and_witness_controlled(
        authored_unknown_writer_graph(),
        "authored-unknown-writer-endpoints",
        WRITER_OFFSET.len(),
        None,
        Some(WRITER_OFFSET),
    );
}
