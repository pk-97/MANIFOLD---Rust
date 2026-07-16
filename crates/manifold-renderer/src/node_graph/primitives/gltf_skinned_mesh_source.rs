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

        if let Some(staging) = &self.staging_verts {
            let copy_size = self.staging_len_bytes.min(dst_verts.size);
            if copy_size > 0 {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst_verts, copy_size);
            }
        }
        if let Some(staging) = &self.staging_joints {
            let n = self.cached_verts.len().min(capacity as usize);
            let copy_size = ((n * std::mem::size_of::<Vec4Vertex>()) as u64).min(dst_joints.size);
            if copy_size > 0 {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst_joints, copy_size);
            }
        }
        if let Some(staging) = &self.staging_weights {
            let n = self.cached_verts.len().min(capacity as usize);
            let copy_size = ((n * std::mem::size_of::<Vec4Vertex>()) as u64).min(dst_weights.size);
            if copy_size > 0 {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst_weights, copy_size);
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
