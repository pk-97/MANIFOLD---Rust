//! `node.gltf_morph_deltas_source` — read a `.glb`/`.gltf` file's sole
//! mesh-owning node contributing a given material index and emit its
//! morph-target POSITION/NORMAL deltas as a flattened target-major
//! `Array(MeshVertex)`.
//!
//! GLTF_ANIMATION_DESIGN.md A3: a sibling of `node.gltf_skinned_mesh_source`
//! (same background-thread parse + staging-buffer-per-frame-blit pattern),
//! NOT an extension of `node.gltf_mesh_source` — deltas are per-vertex
//! PER-TARGET data (`target_count * vertex_count` elements), the same
//! vertex-scale class of data A2 keeps out of baked import-time Tables
//! (`node.gltf_skinned_mesh_source` exists for the identical reason: a
//! per-vertex JOINTS_0/WEIGHTS_0 buffer doesn't belong in JSON either).
//! The base mesh itself still comes from the EXISTING
//! `node.gltf_mesh_source` (unlike skinning, a morphed object's base
//! geometry IS positioned by ordinary node transforms) — this primitive
//! supplies only the additive per-target deltas that
//! `node.morph_targets_blend` sums on top of it.
//!
//! `deltas[target_index * vertex_count + vertex_index]` — the layout
//! `node.morph_targets_blend`'s `deltas` input expects (matches
//! `gltf_load::load_gltf_morph_deltas`'s output exactly, since this
//! primitive is a thin background-thread wrapper around that function).

use std::borrow::Cow;
use std::sync::mpsc;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_load::{DEFAULT_MATERIAL_MESH_PARAM, DEFAULT_MATERIAL_SENTINEL, load_gltf_morph_deltas};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: GltfMorphDeltasSource,
    type_id: "node.gltf_morph_deltas_source",
    purpose: "Read a glTF/.glb file's sole mesh-owning node contributing `material_index` and emit its morph-target POSITION/NORMAL deltas as a flattened target-major Array(MeshVertex) — deltas[target_index * vertex_count + vertex_index]. Wire into node.morph_targets_blend's `deltas` input alongside node.gltf_mesh_source's base geometry (`in`) and node.gltf_morph_weights' sampled weights.",
    inputs: {},
    outputs: {
        deltas: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("path"),
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("material_index"),
            label: "Material Index",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            // BUG-207: -2 is GLB_XFAIL_BURNDOWN_DESIGN.md D4's reserved
            // sentinel (`gltf_load::DEFAULT_MATERIAL_MESH_PARAM`) selecting
            // the sole mesh-owning node contributing the glTF default
            // (materialless) material — real glTF material indices are
            // always >= 0, so widening the range down to -2 costs nothing
            // for every existing selection.
            range: Some((-2.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(200000.0),
            range: Some((1.0, 16000000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "path comes via presetMetadata.stringBindings, same convention as node.gltf_mesh_source/node.gltf_skinned_mesh_source. max_capacity is the pre-allocation ceiling in delta ELEMENTS (target_count * vertex_count, not just vertex_count) — gltf_import.rs sets it from the parsed target_count * the object's vertex_count.",
    examples: [],
    picker: { label: "glTF Morph Deltas", category: Atom },
    summary: "Loads an imported glTF asset's morph-target position/normal deltas, ready to be blended onto its base mesh by a Morph Targets Blend node.",
    category: Geometry3D,
    role: Source,
    aliases: ["gltf morph deltas", "morph target source", "blend shape deltas"],
    boundary_reason: IoBridge,
    extra_fields: {
        // (path, material_index) last parsed (or in flight).
        last_key: (String, i32) = (String::new(), i32::MIN),
        cached_deltas: Vec<MeshVertex> = Vec::new(),
        staging: Option<manifold_gpu::GpuBuffer> = None,
        staging_len_bytes: u64 = 0,
        pending_load: Option<mpsc::Receiver<Result<(Vec<MeshVertex>, u32, u32), String>>> = None,
        uploaded: bool = false,
        // Bumped every time a background parse lands — see
        // `gltf_mesh_source`'s identical field for why this (not
        // `last_key`) is the correct content-change signal.
        // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1.
        content_version: u64 = 0,
        last_copied_content_version: u64 = u64::MAX,
        last_copied_dst_identity: usize = 0,
    },
}

impl Primitive for GltfMorphDeltasSource {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let material_index = match ctx.params.get("material_index") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => 0,
        };

        let key = (path.clone(), material_index);
        if key != self.last_key && self.pending_load.is_none() {
            self.last_key = key;
            self.cached_deltas.clear();
            self.staging = None;
            self.staging_len_bytes = 0;
            self.uploaded = false;
            if !path.is_empty() && (material_index >= 0 || material_index == DEFAULT_MATERIAL_MESH_PARAM) {
                // BUG-207: translate the "-2" param sentinel back to the
                // `gltf_load` sentinel `load_gltf_morph_deltas` expects —
                // same two-sentinel-space split `gltf_mesh_source.rs` uses
                // (`-1` stays the param's own "unset" default).
                let load_material_index = if material_index == DEFAULT_MATERIAL_MESH_PARAM {
                    DEFAULT_MATERIAL_SENTINEL
                } else {
                    material_index as u32
                };
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = load_gltf_morph_deltas(&path_buf, load_material_index);
                    let _ = tx.send(result);
                });
                self.pending_load = Some(rx);
            }
        }

        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok((deltas, _target_count, _vertex_count))) => {
                    self.cached_deltas = deltas;
                    self.uploaded = false;
                    self.content_version = self.content_version.wrapping_add(1);
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_morph_deltas_source: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_morph_deltas_source: background load channel disconnected");
                }
            }
        }

        let Some(dst) = ctx.outputs.array("deltas") else {
            return;
        };
        let capacity = dst.size / std::mem::size_of::<MeshVertex>() as u64;

        if self.cached_deltas.is_empty() {
            return;
        }

        if !self.uploaded {
            let n = self.cached_deltas.len().min(capacity as usize);
            if self.cached_deltas.len() > capacity as usize {
                log::warn!(
                    "node.gltf_morph_deltas_source: {} delta elements, capacity {} — truncating",
                    self.cached_deltas.len(),
                    capacity
                );
            }
            let bytes = &self.cached_deltas[..n];
            let len_bytes = (n * std::mem::size_of::<MeshVertex>()) as u64;
            let device = ctx.gpu_encoder().device;
            let staging = device.create_buffer_shared(len_bytes.max(1));
            unsafe {
                staging.write(0, bytemuck::cast_slice(bytes));
            }
            self.staging = Some(staging);
            self.staging_len_bytes = len_bytes;
            self.uploaded = true;
        }

        // Gated (RENDER_SCENE_PERF_OPTIMIZATION P1/R1) — see
        // `gltf_mesh_source`'s identical copy gate for the rationale.
        if let Some(staging) = &self.staging {
            let dst_identity = dst.identity_key();
            let unchanged = self.content_version == self.last_copied_content_version
                && dst_identity == self.last_copied_dst_identity;
            if unchanged {
                ctx.mark_outputs_unchanged();
            } else {
                let copy_size = self.staging_len_bytes.min(dst.size);
                if copy_size > 0 {
                    ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst, copy_size);
                }
                self.last_copied_content_version = self.content_version;
                self.last_copied_dst_identity = dst_identity;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::ports::{ArrayType, PortType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_one_array_output() {
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(GltfMorphDeltasSource::TYPE_ID, "node.gltf_morph_deltas_source");
        assert!(GltfMorphDeltasSource::INPUTS.is_empty());
        assert_eq!(GltfMorphDeltasSource::OUTPUTS.len(), 1);
        assert_eq!(GltfMorphDeltasSource::OUTPUTS[0].name, "deltas");
        assert_eq!(GltfMorphDeltasSource::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GltfMorphDeltasSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_morph_deltas_source");
    }
}

/// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 gate. Run deliberately:
/// `cargo test -p manifold-renderer --features gpu-proofs
/// node_graph::primitives::gltf_morph_deltas_source::gpu_tests`.
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::ParamValues;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{FrameTime, MetalBackend};
    use crate::TestDevice;
    use manifold_core::{Beats, Seconds};

    const CAPACITY: u32 = 4_000;

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    fn morph_cube_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/AnimatedMorphCube.glb")
    }

    fn params_at(path: &str, material_index: f32, capacity: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(Cow::Borrowed("path"), ParamValue::String(path.to_string().into()));
        p.insert(Cow::Borrowed("material_index"), ParamValue::Float(material_index));
        p.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(capacity));
        p
    }

    fn run_once(
        prim: &mut GltfMorphDeltasSource,
        backend: &MetalBackend,
        device: &TestDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
        time: FrameTime,
    ) -> bool {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(&[], backend_ref, &[]);
        let outputs = NodeOutputs::new(
            output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
        );
        let mut native_enc = device.create_encoder("gltf-morph-deltas-source-test");
        let unchanged;
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
            let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, Some(&mut gpu));
            prim.run(&mut ctx);
            unchanged = ctx.outputs_unchanged;
        }
        native_enc.commit_and_wait_completed();
        unchanged
    }

    fn readback(backend: &MetalBackend, slot: Slot) -> Vec<u8> {
        let buf = backend.array_buffer(slot).expect("array buffer retained");
        let ptr = buf.mapped_ptr().expect("shared buffer");
        unsafe { std::slice::from_raw_parts(ptr, buf.size() as usize) }.to_vec()
    }

    fn settle(
        prim: &mut GltfMorphDeltasSource,
        backend: &MetalBackend,
        device: &TestDevice,
        output_scratch: &[(&'static str, Slot)],
        params: &ParamValues,
    ) {
        for _ in 0..200 {
            run_once(prim, backend, device, output_scratch, params, frame_time());
            if !prim.cached_deltas.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("gltf_morph_deltas_source: parse never settled");
    }

    fn make_buffer_backend(device: &TestDevice) -> (MetalBackend, ResourceId, Slot) {
        let mut backend = MetalBackend::new(device.arc(), 64, 64, manifold_gpu::GpuTextureFormat::Rgba8Unorm);
        let r_out = ResourceId(0);
        let buf =
            device.create_buffer_shared((CAPACITY as u64) * std::mem::size_of::<MeshVertex>() as u64);
        let slot = backend.pre_bind_array(r_out, buf);
        (backend, r_out, slot)
    }

    #[test]
    fn frame2_matches_frame1_on_static_asset_and_declares_unchanged() {
        let path = morph_cube_fixture_path();
        if !path.exists() {
            println!("frame2_matches_frame1_on_static_asset_and_declares_unchanged: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();
        let (backend, _r_out, slot) = make_buffer_backend(&device);
        let scratch: Vec<(&'static str, Slot)> = vec![("deltas", slot)];

        let params = params_at(path.to_str().unwrap(), 0.0, CAPACITY as f32);
        let mut prim = GltfMorphDeltasSource::new();
        settle(&mut prim, &backend, &device, &scratch, &params);
        let frame1 = readback(&backend, slot);

        let unchanged = run_once(&mut prim, &backend, &device, &scratch, &params, frame_time());
        assert!(unchanged, "settled static frame must declare mark_outputs_unchanged");
        let frame2 = readback(&backend, slot);
        assert_eq!(frame1, frame2, "frame 2 must be bit-identical to frame 1 on a static asset");
    }
}
