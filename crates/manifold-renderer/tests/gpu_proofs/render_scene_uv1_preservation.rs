//! Production buffer-codegen proof for MeshVertex UV1 preservation.
//!
//! The morph kernel is the shipping generated path (`wgsl_body` wrapped by
//! `standalone_for_spec`), rather than the hand-written parity oracle. A
//! nonzero UV1 value is written into the source vertex, dispatched through
//! Metal, and read back from the output buffer.

use manifold_gpu::GpuBinding;
use manifold_renderer::generators::mesh_common::MeshVertex;
use manifold_renderer::node_graph::freeze::codegen::{standalone_for_spec, ENTRY};
use manifold_renderer::node_graph::primitives::MorphMesh;

use crate::harness;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MorphUniforms {
    t: f32,
    blend_frames: u32,
    weights_len: u32,
    dispatch_count: u32,
}

fn dispatch_production_morph(source: MeshVertex, target: MeshVertex) -> MeshVertex {
    let h = harness::shared();
    let wgsl = standalone_for_spec::<MorphMesh>().expect("production morph WGSL must generate");
    let pipeline = h
        .device
        .create_compute_pipeline(&wgsl, ENTRY, "uv1-preservation-morph");

    let source_buf = h.device.create_buffer_shared(std::mem::size_of::<MeshVertex>() as u64);
    let target_buf = h.device.create_buffer_shared(std::mem::size_of::<MeshVertex>() as u64);
    let weights_buf = h.device.create_buffer_shared(4);
    let output_buf = h.device.create_buffer_shared(std::mem::size_of::<MeshVertex>() as u64);
    unsafe {
        source_buf.write(0, bytemuck::bytes_of(&source));
        target_buf.write(0, bytemuck::bytes_of(&target));
        weights_buf.write(0, bytemuck::bytes_of(&0.0f32));
    }

    let uniforms = MorphUniforms {
        t: 0.75,
        blend_frames: 0,
        weights_len: 0,
        dispatch_count: 1,
    };
    let bindings = [
        GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
        GpuBinding::Buffer { binding: 1, buffer: &source_buf, offset: 0 },
        GpuBinding::Buffer { binding: 2, buffer: &target_buf, offset: 0 },
        GpuBinding::Buffer { binding: 3, buffer: &weights_buf, offset: 0 },
        GpuBinding::Buffer { binding: 4, buffer: &output_buf, offset: 0 },
    ];
    let mut encoder = h.device.create_encoder("uv1-preservation-morph");
    encoder.dispatch_compute(&pipeline, &bindings, [1, 1, 1], "uv1-preservation-morph");
    encoder.commit_and_wait_completed();

    let ptr = output_buf.mapped_ptr().expect("production morph output must be mapped");
    unsafe { *(ptr as *const MeshVertex) }
}

#[test]
fn production_morph_preserves_nonzero_uv1() {
    let source = MeshVertex {
        position: [0.0, 0.0, 0.0],
        _pad0: 0.0,
        normal: [0.0, 1.0, 0.0],
        _pad1: 0.0,
        uv: [0.1, 0.2],
        _pad2: [0.37, 0.83],
        tangent: [0.0, 0.0, 0.0, 0.0],
        color: [0.2, 0.4, 0.6, 0.8],
    };
    let target = MeshVertex {
        position: [2.0, 3.0, 4.0],
        _pad0: 0.0,
        normal: [0.0, 0.0, 1.0],
        _pad1: 0.0,
        uv: [0.9, 0.8],
        _pad2: [0.91, 0.13],
        tangent: [0.0, 0.0, 0.0, 0.0],
        color: [0.9, 0.7, 0.5, 1.0],
    };

    let output = dispatch_production_morph(source, target);
    assert_eq!(output._pad2, source._pad2, "morph must preserve source TEXCOORD_1");
    assert_eq!(output.color, source.color, "morph must preserve source COLOR_0");
}
