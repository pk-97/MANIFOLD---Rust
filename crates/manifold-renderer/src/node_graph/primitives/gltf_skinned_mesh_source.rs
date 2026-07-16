//! `node.gltf_skinned_mesh_source` — read a `.glb`/`.gltf` file's ONE
//! skinned mesh-owning node for a given material index and emit its
//! LOCAL-space (bind-pose) geometry plus coincident per-vertex JOINTS_0/
//! WEIGHTS_0 arrays, so `node.skin_mesh` can deform it per frame.
//!
//! GLTF_ANIMATION_DESIGN.md A2 (D2): a sibling of `node.gltf_mesh_source`
//! (same background-thread parse + staging-buffer-per-frame-blit
//! pattern), NOT an extension of it — that primitive's Material selector
//! world-transforms vertices by the contributing node's own bind matrix,
//! which is WRONG for a skinned mesh (glTF 2.0 §3.7.3.3: a skinned mesh's
//! positioning comes entirely from the joint hierarchy; the mesh-owning
//! node's own transform is ignored). This primitive never applies a node
//! transform — `gltf_load::load_gltf_skinned_mesh` returns raw local-space
//! vertices, and `node.skin_mesh` supplies ALL positioning via the joint
//! palette `node.gltf_skeleton_pose` computes.
//!
//! `material_index` selects the sole skinned node contributing that
//! material's geometry (the same single-node-per-material scope boundary
//! `node.gltf_animation_source`'s import wiring uses) — `gltf_import.rs`
//! only wires this primitive when `GltfMaterialInfo::skin` resolved.

use std::borrow::Cow;
use std::sync::mpsc;

use crate::generators::mesh_common::{MeshVertex, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_load::load_gltf_skinned_mesh;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: GltfSkinnedMeshSource,
    type_id: "node.gltf_skinned_mesh_source",
    purpose: "Read a glTF/.glb file's sole skinned mesh-owning node contributing `material_index` and emit its LOCAL-space (bind-pose) geometry as Array(MeshVertex), plus coincident per-vertex JOINTS_0 (as f32 indices) and WEIGHTS_0 arrays. NO node transform is applied (glTF skinning ignores the mesh node's own transform) — wire `vertices`/`joints`/`weights` into node.skin_mesh along with a node.gltf_skeleton_pose's joint-matrix palette to deform the mesh per frame.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
        joints: Array(Vec4Vertex),
        weights: Array(Vec4Vertex),
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
            range: Some((0.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(200000.0),
            range: Some((36.0, 8000000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "path comes via presetMetadata.stringBindings, same convention as node.gltf_mesh_source. max_capacity is the pre-allocation ceiling in vertices for all three arrays (they stay coincident); gltf_import.rs sets it to the exact parsed vertex count.",
    examples: [],
    picker: { label: "glTF Skinned Mesh", category: Atom },
    summary: "Loads an imported glTF character's bind-pose geometry and its per-vertex joint weights, ready to be deformed by a Skin Mesh node.",
    category: Geometry3D,
    role: Source,
    aliases: ["gltf skinned", "skinned mesh source", "rig mesh"],
    boundary_reason: IoBridge,
    extra_fields: {
        // (path, material_index) last parsed (or in flight).
        last_key: (String, i32) = (String::new(), i32::MIN),
        cached_verts: Vec<MeshVertex> = Vec::new(),
        cached_joints: Vec<[f32; 4]> = Vec::new(),
        cached_weights: Vec<[f32; 4]> = Vec::new(),
        staging_verts: Option<manifold_gpu::GpuBuffer> = None,
        staging_joints: Option<manifold_gpu::GpuBuffer> = None,
        staging_weights: Option<manifold_gpu::GpuBuffer> = None,
        staging_len_bytes: u64 = 0,
        pending_load: Option<mpsc::Receiver<Result<(Vec<MeshVertex>, Vec<[f32; 4]>, Vec<[f32; 4]>), String>>> = None,
        uploaded: bool = false,
        // Bumped every time a background parse lands — see
        // `gltf_mesh_source`'s identical field for why this (not
        // `last_key`) is the correct content-change signal. Shared across
        // all three outputs since they always update together (one parse
        // produces all three). RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md
        // P1/R1.
        content_version: u64 = 0,
        last_copied_content_version: u64 = u64::MAX,
        // Per-output dst identity — the three outputs are separate
        // physical buffers and may be recycled independently.
        last_copied_verts_identity: usize = 0,
        last_copied_joints_identity: usize = 0,
        last_copied_weights_identity: usize = 0,
    },
}

impl Primitive for GltfSkinnedMeshSource {
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
            self.cached_verts.clear();
            self.cached_joints.clear();
            self.cached_weights.clear();
            self.staging_verts = None;
            self.staging_joints = None;
            self.staging_weights = None;
            self.staging_len_bytes = 0;
            self.uploaded = false;
            if !path.is_empty() && material_index >= 0 {
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = load_gltf_skinned_mesh(&path_buf, material_index as u32);
                    let _ = tx.send(result);
                });
                self.pending_load = Some(rx);
            }
        }

        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok((verts, joints, weights))) => {
                    self.cached_verts = verts;
                    self.cached_joints = joints;
                    self.cached_weights = weights;
                    self.uploaded = false;
                    self.content_version = self.content_version.wrapping_add(1);
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_skinned_mesh_source: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_skinned_mesh_source: background load channel disconnected");
                }
            }
        }

        let Some(dst_verts) = ctx.outputs.array("vertices") else {
            return;
        };
        let Some(dst_joints) = ctx.outputs.array("joints") else {
            return;
        };
        let Some(dst_weights) = ctx.outputs.array("weights") else {
            return;
        };
        let capacity = dst_verts.size / std::mem::size_of::<MeshVertex>() as u64;

        if self.cached_verts.is_empty() {
            return;
        }

        if !self.uploaded {
            let n = self.cached_verts.len().min(capacity as usize);
            if self.cached_verts.len() > capacity as usize {
                log::warn!(
                    "node.gltf_skinned_mesh_source: mesh has {} vertices, capacity {} — truncating",
                    self.cached_verts.len(),
                    capacity
                );
            }
            let device = ctx.gpu_encoder().device;
            let vert_bytes = &self.cached_verts[..n];
            let vert_len = (n * std::mem::size_of::<MeshVertex>()) as u64;
            let sv = device.create_buffer_shared(vert_len.max(1));
            unsafe {
                sv.write(0, bytemuck::cast_slice(vert_bytes));
            }
            self.staging_verts = Some(sv);
            self.staging_len_bytes = vert_len;

            let joints_vec4: Vec<Vec4Vertex> =
                self.cached_joints[..n].iter().map(|j| Vec4Vertex { position: *j }).collect();
            let joints_len = (n * std::mem::size_of::<Vec4Vertex>()) as u64;
            let sj = device.create_buffer_shared(joints_len.max(1));
            unsafe {
                sj.write(0, bytemuck::cast_slice(&joints_vec4));
            }
            self.staging_joints = Some(sj);

            let weights_vec4: Vec<Vec4Vertex> =
                self.cached_weights[..n].iter().map(|w| Vec4Vertex { position: *w }).collect();
            let weights_len = (n * std::mem::size_of::<Vec4Vertex>()) as u64;
            let sw = device.create_buffer_shared(weights_len.max(1));
            unsafe {
                sw.write(0, bytemuck::cast_slice(&weights_vec4));
            }
            self.staging_weights = Some(sw);

            self.uploaded = true;
        }

        // Gated (RENDER_SCENE_PERF_OPTIMIZATION P1/R1): skip each output's
        // copy independently when `content_version` hasn't changed since
        // its last completed copy AND its dst is the same physical buffer
        // as last frame (pool recycle can hand back a different physical
        // buffer per-output, independently of the others). `mark_outputs_
        // unchanged()` declares ALL THREE outputs unchanged, so it is only
        // called when every one of the three copies was skipped — I3
        // forbids declaring unchanged when even one output actually got a
        // fresh write this frame.
        let content_unchanged = self.content_version == self.last_copied_content_version;
        let mut any_copy_ran = false;

        if let Some(staging) = &self.staging_verts {
            let dst_identity = dst_verts.identity_key();
            if content_unchanged && dst_identity == self.last_copied_verts_identity {
                // skip
            } else {
                let copy_size = self.staging_len_bytes.min(dst_verts.size);
                if copy_size > 0 {
                    ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst_verts, copy_size);
                }
                self.last_copied_verts_identity = dst_identity;
                any_copy_ran = true;
            }
        }
        if let Some(staging) = &self.staging_joints {
            let dst_identity = dst_joints.identity_key();
            if content_unchanged && dst_identity == self.last_copied_joints_identity {
                // skip
            } else {
                let n = self.cached_verts.len().min(capacity as usize);
                let copy_size = ((n * std::mem::size_of::<Vec4Vertex>()) as u64).min(dst_joints.size);
                if copy_size > 0 {
                    ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst_joints, copy_size);
                }
                self.last_copied_joints_identity = dst_identity;
                any_copy_ran = true;
            }
        }
        if let Some(staging) = &self.staging_weights {
            let dst_identity = dst_weights.identity_key();
            if content_unchanged && dst_identity == self.last_copied_weights_identity {
                // skip
            } else {
                let n = self.cached_verts.len().min(capacity as usize);
                let copy_size = ((n * std::mem::size_of::<Vec4Vertex>()) as u64).min(dst_weights.size);
                if copy_size > 0 {
                    ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst_weights, copy_size);
                }
                self.last_copied_weights_identity = dst_identity;
                any_copy_ran = true;
            }
        }

        self.last_copied_content_version = self.content_version;
        if !any_copy_ran {
            ctx.mark_outputs_unchanged();
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
    fn declares_zero_inputs_and_three_coincident_array_outputs() {
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let vec4_layout = ArrayType::of_known::<Vec4Vertex>();
        assert_eq!(GltfSkinnedMeshSource::TYPE_ID, "node.gltf_skinned_mesh_source");
        assert!(GltfSkinnedMeshSource::INPUTS.is_empty());
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS.len(), 3);
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS[0].name, "vertices");
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS[0].ty, PortType::Array(mesh_layout));
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS[1].name, "joints");
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS[1].ty, PortType::Array(vec4_layout));
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS[2].name, "weights");
        assert_eq!(GltfSkinnedMeshSource::OUTPUTS[2].ty, PortType::Array(vec4_layout));
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GltfSkinnedMeshSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_skinned_mesh_source");
    }
}

/// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 gate. Run deliberately:
/// `cargo test -p manifold-renderer --features gpu-proofs
/// node_graph::primitives::gltf_skinned_mesh_source::gpu_tests`.
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

    const CAPACITY: u32 = 20_000;

    fn frame_time_at(seconds: f64) -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(seconds),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn cesium_man_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/khronos/CesiumMan.glb")
    }

    fn params_at(path: &str, material_index: f32) -> ParamValues {
        let mut p = ahash::AHashMap::default();
        p.insert(Cow::Borrowed("path"), ParamValue::String(path.to_string().into()));
        p.insert(Cow::Borrowed("material_index"), ParamValue::Float(material_index));
        p.insert(Cow::Borrowed("max_capacity"), ParamValue::Float(CAPACITY as f32));
        p
    }

    #[allow(clippy::too_many_arguments)]
    fn run_once(
        prim: &mut GltfSkinnedMeshSource,
        backend: &MetalBackend,
        device: &TestDevice,
        verts_slot: Slot,
        joints_slot: Slot,
        weights_slot: Slot,
        params: &ParamValues,
        time: FrameTime,
    ) -> bool {
        let output_scratch: Vec<(&'static str, Slot)> =
            vec![("vertices", verts_slot), ("joints", joints_slot), ("weights", weights_slot)];
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(&[], backend_ref, &[]);
        let outputs = NodeOutputs::new(
            &output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
        );
        let mut native_enc = device.create_encoder("gltf-skinned-mesh-source-test");
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

    #[allow(clippy::too_many_arguments)]
    fn settle(
        prim: &mut GltfSkinnedMeshSource,
        backend: &MetalBackend,
        device: &TestDevice,
        verts_slot: Slot,
        joints_slot: Slot,
        weights_slot: Slot,
        params: &ParamValues,
    ) {
        for _ in 0..200 {
            run_once(prim, backend, device, verts_slot, joints_slot, weights_slot, params, frame_time_at(0.0));
            if !prim.cached_verts.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("gltf_skinned_mesh_source: parse never settled");
    }

    fn make_buffer_backend(device: &TestDevice) -> (MetalBackend, Slot, Slot, Slot) {
        let mut backend = MetalBackend::new(device.arc(), 64, 64, manifold_gpu::GpuTextureFormat::Rgba8Unorm);
        let verts_buf =
            device.create_buffer_shared((CAPACITY as u64) * std::mem::size_of::<MeshVertex>() as u64);
        let joints_buf =
            device.create_buffer_shared((CAPACITY as u64) * std::mem::size_of::<Vec4Vertex>() as u64);
        let weights_buf =
            device.create_buffer_shared((CAPACITY as u64) * std::mem::size_of::<Vec4Vertex>() as u64);
        let verts_slot = backend.pre_bind_array(ResourceId(0), verts_buf);
        let joints_slot = backend.pre_bind_array(ResourceId(1), joints_buf);
        let weights_slot = backend.pre_bind_array(ResourceId(2), weights_buf);
        (backend, verts_slot, joints_slot, weights_slot)
    }

    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P1/R1 gate: on a static
    /// asset, frame 2's three outputs are bit-identical to frame 1's, and
    /// the copy skip (`mark_outputs_unchanged`) fires on frame 2.
    #[test]
    fn frame2_matches_frame1_on_static_asset_and_declares_unchanged() {
        let path = cesium_man_fixture_path();
        if !path.exists() {
            println!("frame2_matches_frame1_on_static_asset_and_declares_unchanged: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();
        let (backend, vs, js, ws) = make_buffer_backend(&device);
        let params = params_at(path.to_str().unwrap(), 0.0);
        let mut prim = GltfSkinnedMeshSource::new();
        settle(&mut prim, &backend, &device, vs, js, ws, &params);

        let f1_verts = readback(&backend, vs);
        let f1_joints = readback(&backend, js);
        let f1_weights = readback(&backend, ws);

        let unchanged = run_once(&mut prim, &backend, &device, vs, js, ws, &params, frame_time_at(0.0));
        assert!(unchanged, "settled static frame must declare mark_outputs_unchanged");

        assert_eq!(f1_verts, readback(&backend, vs), "verts must be bit-identical frame 2 vs frame 1");
        assert_eq!(f1_joints, readback(&backend, js), "joints must be bit-identical frame 2 vs frame 1");
        assert_eq!(f1_weights, readback(&backend, ws), "weights must be bit-identical frame 2 vs frame 1");
    }

    /// D9's "prove it, don't trust the sentence" test: this primitive
    /// supplies only the LOCAL-space bind-pose geometry + weights — never
    /// the animated pose (that's `node.gltf_skeleton_pose` / `node.skin_mesh`,
    /// files this phase never touches). Confirm the P1 gate here is
    /// correctly time-independent: on an animated skinned fixture, this
    /// primitive's own output settles once and then correctly stays
    /// gated (mark_outputs_unchanged) across DIFFERENT playhead positions,
    /// because its bind-pose output never depends on `ctx.time` — the
    /// per-frame pose animation itself happens entirely downstream, outside
    /// this phase's touched files, so it is unaffected by this gate by
    /// construction (this primitive never reads `ctx.time`).
    #[test]
    fn gate_is_unaffected_by_playhead_on_an_animated_fixture() {
        let path = cesium_man_fixture_path();
        if !path.exists() {
            println!("gate_is_unaffected_by_playhead_on_an_animated_fixture: fixture not found at {}, skipping", path.display());
            return;
        }
        let device = crate::test_device();
        let (backend, vs, js, ws) = make_buffer_backend(&device);
        let params = params_at(path.to_str().unwrap(), 0.0);
        let mut prim = GltfSkinnedMeshSource::new();
        settle(&mut prim, &backend, &device, vs, js, ws, &params);
        let settled_verts = readback(&backend, vs);

        // Advance the playhead across several different times — this
        // primitive's bind-pose output must stay gated (unchanged) and
        // byte-identical at every one of them, since the actual animation
        // sampling happens in gltf_skeleton_pose/skin_mesh, not here.
        for t in [0.1, 0.5, 1.0, 2.37] {
            let unchanged = run_once(&mut prim, &backend, &device, vs, js, ws, &params, frame_time_at(t));
            assert!(unchanged, "bind-pose source must gate regardless of playhead position (t={t})");
            assert_eq!(
                settled_verts,
                readback(&backend, vs),
                "bind-pose output must not vary with the playhead (t={t})"
            );
        }
    }
}
