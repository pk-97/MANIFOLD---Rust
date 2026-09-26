//! GPU proof that stock fragment masks remain continuous through cut remapping.
//!
//! The fixture is deliberately a triangle list: two adjacent triangles share
//! two source vertices but have different centroids.  A centroid-sampled mask
//! therefore gives the shared vertices different weights, while vertex
//! sampling gives the same weight before the cut map interpolates it.

#![allow(clippy::too_many_arguments)]

use crate::generators::mesh_common::{MeshVertex, Vec4Vertex};
use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
use crate::node_graph::primitives::{
    MeshSpatialMask, MeshStaggerEnvelope, MorphMesh, RemapCutWeights, RemapMeshCut,
};
use manifold_gpu::GpuBinding;
use serde_json::Value;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskUniforms {
    shape: u32,
    sample_mode: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    yaw: f32,
    pitch: f32,
    width: f32,
    feather: f32,
    invert: f32,
    amount: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    cell_size: f32,
    low: f32,
    high: f32,
    weights_len: u32,
    dispatch_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StaggerUniforms {
    sample_mode: u32,
    elapsed_beats: f32,
    attack_beats: f32,
    hold_beats: f32,
    release_beats: f32,
    stagger_beats: f32,
    amount: f32,
    yaw: f32,
    pitch: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

fn vertex(position: [f32; 3]) -> MeshVertex {
    MeshVertex {
        position,
        _pad0: 0.0,
        normal: [0.0, 0.0, 1.0],
        _pad1: 0.0,
        uv: [0.0, 0.0],
        _pad2: [0.0; 2],
        tangent: [0.0, 0.0, 0.0, 1.0],
        color: [1.0; 4],
    }
}

fn map(bary: [f32; 3], triangle: u32) -> Vec4Vertex {
    Vec4Vertex {
        position: [bary[0], bary[1], bary[2], triangle as f32],
    }
}

fn source_mesh() -> Vec<MeshVertex> {
    // Triangle 0 is A-B-C; triangle 1 is B-A-D.  A and B are shared, while
    // the shared edge crosses the feather and the centroids differ.
    vec![
        vertex([-0.6, 0.3, 0.0]),
        vertex([0.65, 0.4, 0.0]),
        vertex([0.0, -0.60, 0.0]),
        vertex([0.65, 0.4, 0.0]),
        vertex([-0.6, 0.3, 0.0]),
        vertex([0.0, 1.50, 0.0]),
    ]
}

fn shifted_fragment_mesh() -> Vec<MeshVertex> {
    let mut mesh = source_mesh();
    // Both source triangles belong to the same moving fragment.
    for vertex in &mut mesh {
        vertex.position[2] = 0.75;
    }
    mesh
}

fn cut_maps() -> Vec<Vec4Vertex> {
    // Each source triangle has duplicate corners and the midpoint of its
    // shared A/B edge, represented independently by each source triangle.
    vec![
        map([1.0, 0.0, 0.0], 0),
        map([0.0, 1.0, 0.0], 0),
        map([0.5, 0.5, 0.0], 0),
        map([1.0, 0.0, 0.0], 0),
        map([0.5, 0.5, 0.0], 0),
        map([0.0, 1.0, 0.0], 0),
        map([0.0, 1.0, 0.0], 1),
        map([1.0, 0.0, 0.0], 1),
        map([0.5, 0.5, 0.0], 1),
        map([0.0, 1.0, 0.0], 1),
        map([0.5, 0.5, 0.0], 1),
        map([1.0, 0.0, 0.0], 1),
    ]
}

fn dispatch_mask(
    device: &manifold_gpu::GpuDevice,
    wgsl: &str,
    source: &[MeshVertex],
    uniforms: MaskUniforms,
    label: &str,
) -> Vec<f32> {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
    let source_buf = device.create_buffer_shared(std::mem::size_of_val(source) as u64);
    let output_buf = device.create_buffer_shared((source.len() * 4) as u64);
    unsafe {
        source_buf.write(0, bytemuck::cast_slice(source));
    }
    let mut encoder = device.create_encoder(label);
    encoder.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &source_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &source_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &output_buf,
                offset: 0,
            },
        ],
        [(source.len() as u32).div_ceil(256), 1, 1],
        label,
    );
    encoder.commit_and_wait_completed();
    let ptr = output_buf.mapped_ptr().expect("mask output");
    unsafe { std::slice::from_raw_parts(ptr as *const f32, source.len()) }.to_vec()
}

fn dispatch_stagger(
    device: &manifold_gpu::GpuDevice,
    wgsl: &str,
    source: &[MeshVertex],
    incoming: &[f32],
    uniforms: StaggerUniforms,
    label: &str,
) -> Vec<f32> {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
    let source_buf = device.create_buffer_shared(std::mem::size_of_val(source) as u64);
    let incoming_buf = device.create_buffer_shared(std::mem::size_of_val(incoming) as u64);
    let output_buf = device.create_buffer_shared((source.len() * 4) as u64);
    unsafe {
        source_buf.write(0, bytemuck::cast_slice(source));
        incoming_buf.write(0, bytemuck::cast_slice(incoming));
    }
    let mut encoder = device.create_encoder(label);
    encoder.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &source_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &incoming_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &output_buf,
                offset: 0,
            },
        ],
        [(source.len() as u32).div_ceil(256), 1, 1],
        label,
    );
    encoder.commit_and_wait_completed();
    let ptr = output_buf.mapped_ptr().expect("stagger output");
    unsafe { std::slice::from_raw_parts(ptr as *const f32, source.len()) }.to_vec()
}

fn dispatch_remap_weights(
    device: &manifold_gpu::GpuDevice,
    wgsl: &str,
    source: &[f32],
    maps: &[Vec4Vertex],
    label: &str,
) -> Vec<f32> {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
    let source_buf = device.create_buffer_shared((source.len() * 4).max(4) as u64);
    let map_buf = device.create_buffer_shared((maps.len() * 16).max(16) as u64);
    let output_buf = device.create_buffer_shared((maps.len() * 4).max(4) as u64);
    unsafe {
        source_buf.write(0, bytemuck::cast_slice(source));
        map_buf.write(0, bytemuck::cast_slice(maps));
    }
    let uniforms = [maps.len() as u32, 0, 0, 0];
    let mut encoder = device.create_encoder(label);
    encoder.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &source_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &map_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &output_buf,
                offset: 0,
            },
        ],
        [(maps.len() as u32).div_ceil(256), 1, 1],
        label,
    );
    encoder.commit_and_wait_completed();
    let ptr = output_buf.mapped_ptr().expect("remapped weights");
    unsafe { std::slice::from_raw_parts(ptr as *const f32, maps.len()) }.to_vec()
}

fn dispatch_remap_mesh(
    device: &manifold_gpu::GpuDevice,
    wgsl: &str,
    source: &[MeshVertex],
    maps: &[Vec4Vertex],
    label: &str,
) -> Vec<MeshVertex> {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
    let source_buf = device.create_buffer_shared(std::mem::size_of_val(source) as u64);
    let map_buf = device.create_buffer_shared((maps.len() * 16) as u64);
    let output_buf =
        device.create_buffer_shared((maps.len() * std::mem::size_of::<MeshVertex>()) as u64);
    unsafe {
        source_buf.write(0, bytemuck::cast_slice(source));
        map_buf.write(0, bytemuck::cast_slice(maps));
    }
    let uniforms = [maps.len() as u32, 0, 0, 0];
    let mut encoder = device.create_encoder(label);
    encoder.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &source_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &map_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &output_buf,
                offset: 0,
            },
        ],
        [(maps.len() as u32).div_ceil(256), 1, 1],
        label,
    );
    encoder.commit_and_wait_completed();
    let ptr = output_buf.mapped_ptr().expect("remapped mesh");
    unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, maps.len()) }.to_vec()
}

fn dispatch_morph(
    device: &manifold_gpu::GpuDevice,
    wgsl: &str,
    input: &[MeshVertex],
    target: &[MeshVertex],
    weights: &[f32],
    t: f32,
    label: &str,
) -> Vec<MeshVertex> {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, label);
    let input_buf = device.create_buffer_shared(std::mem::size_of_val(input) as u64);
    let target_buf = device.create_buffer_shared(std::mem::size_of_val(target) as u64);
    let weights_buf = device.create_buffer_shared((weights.len() * 4).max(4) as u64);
    let output_buf = device.create_buffer_shared(std::mem::size_of_val(input) as u64);
    unsafe {
        input_buf.write(0, bytemuck::cast_slice(input));
        target_buf.write(0, bytemuck::cast_slice(target));
        weights_buf.write(0, bytemuck::cast_slice(weights));
    }
    let uniforms = [t.to_bits(), 1, weights.len() as u32, input.len() as u32];
    let mut encoder = device.create_encoder(label);
    encoder.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: &input_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer: &target_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: &weights_buf,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: &output_buf,
                offset: 0,
            },
        ],
        [(input.len() as u32).div_ceil(256), 1, 1],
        label,
    );
    encoder.commit_and_wait_completed();
    let ptr = output_buf.mapped_ptr().expect("morph output");
    unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, input.len()) }.to_vec()
}

fn stock_param(json: &str, type_id: &str, param: &str) -> u32 {
    fn visit(value: &Value, type_id: &str, param: &str, found: &mut Option<u32>) {
        if let Value::Object(object) = value {
            if object.get("typeId").and_then(Value::as_str) == Some(type_id) {
                let value = object
                    .get("params")
                    .and_then(|params| params.get(param))
                    .and_then(|entry| entry.get("value"))
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| panic!("{type_id}.{param} must have an integer value"));
                assert!(
                    found.replace(value as u32).is_none(),
                    "duplicate {type_id}.{param}"
                );
            }
            for value in object.values() {
                visit(value, type_id, param, found);
            }
        } else if let Value::Array(values) = value {
            for value in values {
                visit(value, type_id, param, found);
            }
        }
    }
    let value: Value = serde_json::from_str(json).expect("stock recipe JSON");
    let mut found = None;
    visit(&value, type_id, param, &mut found);
    found.unwrap_or_else(|| panic!("stock recipe lacks {type_id}.{param}"))
}

fn mask_uniforms(shape: u32, sample_mode: u32, count: usize) -> MaskUniforms {
    let (width, feather) = if shape == 1 {
        (0.35, 0.60)
    } else {
        (0.10, 0.50)
    };
    MaskUniforms {
        shape,
        sample_mode,
        center_x: 0.0,
        center_y: 0.0,
        center_z: 0.0,
        yaw: 0.0,
        pitch: 0.0,
        width,
        feather,
        invert: 0.0,
        amount: 1.0,
        scale: 1.0,
        source_offset_x: 0.0,
        source_offset_y: 0.0,
        source_offset_z: 0.0,
        cell_size: 0.2,
        low: 0.0,
        high: 1.0,
        weights_len: 0,
        dispatch_count: count as u32,
    }
}

fn stagger_uniforms(sample_mode: u32, count: usize) -> StaggerUniforms {
    StaggerUniforms {
        sample_mode,
        elapsed_beats: 0.65,
        attack_beats: 0.80,
        hold_beats: 0.0,
        release_beats: 1.10,
        stagger_beats: 0.90,
        amount: 1.0,
        yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        source_offset_x: 0.0,
        source_offset_y: 0.0,
        source_offset_z: 0.0,
        weights_len: count as u32,
        dispatch_count: count as u32,
        _pad0: 0,
    }
}

fn assert_close(a: f32, b: f32, what: &str) {
    assert!((a - b).abs() <= 1e-6, "{what}: {a} != {b}");
}

fn assert_continuous_after_morph(
    device: &manifold_gpu::GpuDevice,
    source: &[MeshVertex],
    target: &[MeshVertex],
    maps: &[Vec4Vertex],
    source_weights: &[f32],
    remap_weights_wgsl: &str,
    remap_mesh_wgsl: &str,
    morph_wgsl: &str,
    label: &str,
) {
    let weights = dispatch_remap_weights(device, remap_weights_wgsl, source_weights, maps, label);
    let remapped_source = dispatch_remap_mesh(device, remap_mesh_wgsl, source, maps, label);
    let remapped_target = dispatch_remap_mesh(device, remap_mesh_wgsl, target, maps, label);
    let output = dispatch_morph(
        device,
        morph_wgsl,
        &remapped_source,
        &remapped_target,
        &weights,
        0.75,
        label,
    );

    for (left, right, name) in [
        (0, 3, "tri0 A duplicate"),
        (1, 5, "tri0 B duplicate"),
        (2, 4, "tri0 edge duplicate"),
        (6, 9, "tri1 A duplicate"),
        (7, 11, "tri1 B duplicate"),
        (8, 10, "tri1 edge duplicate"),
    ] {
        assert_close(weights[left], weights[right], name);
        for axis in 0..3 {
            assert_close(
                output[left].position[axis],
                output[right].position[axis],
                name,
            );
        }
    }
    for (left, right, name) in [
        (0, 6, "A shared edge weight"),
        (1, 7, "B shared edge weight"),
        (2, 8, "shared edge midpoint weight"),
    ] {
        assert_close(weights[left], weights[right], name);
    }
    for (left, right) in [(0, 6), (1, 7), (2, 8)] {
        for axis in 0..3 {
            assert_close(
                output[left].position[axis],
                output[right].position[axis],
                "neighbouring source triangles must remain joined",
            );
        }
    }
    // An intentional cut gives the two sides different target positions.
    // The mask must preserve that separation, not weld the fragments.
    let mut split_target = remapped_target.clone();
    for vertex in &mut split_target[6..] {
        vertex.position[2] += 0.4;
    }
    let split = dispatch_morph(
        device,
        morph_wgsl,
        &remapped_source,
        &split_target,
        &weights,
        0.75,
        label,
    );
    for (left, right) in [(0, 6), (1, 7), (2, 8)] {
        let gap = (split[left].position[2] - split[right].position[2]).abs();
        assert_close(
            gap,
            0.4 * weights[left] * 0.75,
            "intentional cut scales with weight",
        );
        assert!(gap > 1e-5, "intentional cut must remain open: {gap}");
    }
    assert!(
        weights
            .iter()
            .any(|weight| *weight > 1e-4 && *weight < 0.9999),
        "continuity fixture must exercise partial weights: {weights:?}"
    );
}

const MASK_STOCKS: &[(&str, &str)] = &[
    (
        "OrderedRecon",
        include_str!("../../assets/scene-modifier-presets/OrderedRecon.json"),
    ),
    (
        "MaskedPeel",
        include_str!("../../assets/scene-modifier-presets/MaskedPeel.json"),
    ),
    (
        "SurfacePeel",
        include_str!("../../assets/scene-modifier-presets/SurfacePeel.json"),
    ),
    (
        "OrderedReconHit",
        include_str!("../../assets/scene-modifier-presets/OrderedReconHit.json"),
    ),
    (
        "VortexFragments",
        include_str!("../../assets/scene-modifier-presets/VortexFragments.json"),
    ),
];

const RECON_STOCKS: &[(&str, &str)] = &[
    (
        "OrderedRecon",
        include_str!("../../assets/scene-modifier-presets/OrderedRecon.json"),
    ),
    (
        "OrderedReconHit",
        include_str!("../../assets/scene-modifier-presets/OrderedReconHit.json"),
    ),
];

#[test]
fn stock_fragment_masks_are_vertex_continuous_through_cut_remap_and_morph() {
    let device = crate::test_device();
    let source = source_mesh();
    let target = shifted_fragment_mesh();
    let maps = cut_maps();
    let mask_wgsl = standalone_for_spec::<MeshSpatialMask>().expect("mask codegen");
    let remap_weights_wgsl =
        standalone_for_spec::<RemapCutWeights>().expect("weight remap codegen");
    let remap_mesh_wgsl = standalone_for_spec::<RemapMeshCut>().expect("mesh remap codegen");
    let morph_wgsl = standalone_for_spec::<MorphMesh>().expect("morph codegen");

    for (name, recipe) in MASK_STOCKS {
        let sample_mode = stock_param(recipe, "node.mesh_spatial_mask", "sample_mode");
        let stock_shape = stock_param(recipe, "node.mesh_spatial_mask", "shape");
        assert_eq!(sample_mode, 0, "{name} stock mask must use vertex sampling");
        for shape in [stock_shape, 1] {
            let weights = dispatch_mask(
                &device,
                &mask_wgsl,
                &source,
                mask_uniforms(shape, sample_mode, source.len()),
                &format!("{name}-mask-{shape}"),
            );
            assert_close(weights[0], weights[4], &format!("{name} source A"));
            assert_close(weights[1], weights[3], &format!("{name} source B"));
            assert_continuous_after_morph(
                &device,
                &source,
                &target,
                &maps,
                &weights,
                &remap_weights_wgsl,
                &remap_mesh_wgsl,
                &morph_wgsl,
                &format!("{name}-mask-{shape}"),
            );

            let legacy = dispatch_mask(
                &device,
                &mask_wgsl,
                &source,
                mask_uniforms(shape, 1, source.len()),
                &format!("{name}-legacy-centroid-{shape}"),
            );
            assert!(
                (legacy[0] - legacy[4]).abs() > 0.01,
                "{name} legacy sample_mode=1 control must expose a shared-corner crack: {legacy:?}"
            );
        }
    }
}

#[test]
fn recon_stagger_modes_are_vertex_continuous_through_cut_remap_and_morph() {
    let device = crate::test_device();
    let source = source_mesh();
    let target = shifted_fragment_mesh();
    let maps = cut_maps();
    let mask_wgsl = standalone_for_spec::<MeshSpatialMask>().expect("mask codegen");
    let stagger_wgsl = standalone_for_spec::<MeshStaggerEnvelope>().expect("stagger codegen");
    let remap_weights_wgsl =
        standalone_for_spec::<RemapCutWeights>().expect("weight remap codegen");
    let remap_mesh_wgsl = standalone_for_spec::<RemapMeshCut>().expect("mesh remap codegen");
    let morph_wgsl = standalone_for_spec::<MorphMesh>().expect("morph codegen");

    for (name, recipe) in RECON_STOCKS {
        let mask_mode = stock_param(recipe, "node.mesh_spatial_mask", "sample_mode");
        let stagger_mode = stock_param(recipe, "node.mesh_stagger_envelope", "sample_mode");
        assert_eq!(mask_mode, 0, "{name} stock mask must use vertex sampling");
        assert_eq!(
            stagger_mode, 0,
            "{name} stock stagger must use vertex sampling"
        );

        let mask = dispatch_mask(
            &device,
            &mask_wgsl,
            &source,
            mask_uniforms(0, mask_mode, source.len()),
            &format!("{name}-mask"),
        );
        let stagger = dispatch_stagger(
            &device,
            &stagger_wgsl,
            &source,
            &mask,
            StaggerUniforms {
                weights_len: mask.len() as u32,
                ..stagger_uniforms(stagger_mode, source.len())
            },
            &format!("{name}-stagger"),
        );
        assert_close(stagger[0], stagger[4], &format!("{name} source A"));
        assert_close(stagger[1], stagger[3], &format!("{name} source B"));
        assert_continuous_after_morph(
            &device,
            &source,
            &target,
            &maps,
            &stagger,
            &remap_weights_wgsl,
            &remap_mesh_wgsl,
            &morph_wgsl,
            &format!("{name}-stagger"),
        );

        let legacy_mask = dispatch_mask(
            &device,
            &mask_wgsl,
            &source,
            mask_uniforms(0, 1, source.len()),
            &format!("{name}-legacy-mask"),
        );
        let legacy_stagger = dispatch_stagger(
            &device,
            &stagger_wgsl,
            &source,
            &legacy_mask,
            StaggerUniforms {
                sample_mode: 1,
                weights_len: legacy_mask.len() as u32,
                ..stagger_uniforms(1, source.len())
            },
            &format!("{name}-legacy-stagger"),
        );
        assert!(
            (legacy_stagger[0] - legacy_stagger[4]).abs() > 0.01,
            "{name} legacy centroid mask+stagger must expose a shared-corner crack: {legacy_stagger:?}"
        );
    }
}
