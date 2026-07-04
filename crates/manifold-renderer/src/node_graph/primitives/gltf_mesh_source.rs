//! `node.gltf_mesh_source` — read a `.glb`/`.gltf` file from disk and emit
//! its geometry as an `Array(MeshVertex)` wire, so imported meshes flow
//! into `node.render_scene` like any hand-built shape (`node.cube_mesh`,
//! `node.generate_grid_mesh`, …).
//!
//! File I/O + the CPU flatten (`gltf_load::load_gltf_mesh`) happen on a
//! background thread (`std::thread::spawn` + `mpsc::channel`), same
//! pattern as `node.image_folder`, so the content thread never stalls on
//! a multi-megabyte glTF parse. The last successful parse stays resident
//! (`cached_verts`) and re-uploads to a staging buffer only when the
//! parse result actually changes; the GPU copy into the pre-bound output
//! buffer runs every frame via a cheap blit.

use std::sync::mpsc;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_load::{GltfMeshSelector, load_gltf_mesh};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: GltfMeshSource,
    type_id: "node.gltf_mesh_source",
    purpose: "Read a glTF/.glb file from disk and emit its geometry as a triangle-list Array(MeshVertex) wire. mesh_index=-1 (the default) world-combines the whole default scene — drop a model in and it renders. mesh_index >= 0 selects one mesh (optionally one primitive of it via primitive_index) in LOCAL space, undisplaced by node transforms, for callers that place it themselves via node.render_scene's per-object transform.",
    inputs: {},
    outputs: {
        vertices: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "path",
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "mesh_index",
            label: "Mesh Index",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "primitive_index",
            label: "Primitive Index",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 1024.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(200000.0),
            range: Some((36.0, 8000000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "path comes via presetMetadata.stringBindings — wire the JSON-graph generator's outer-card Browse field into this primitive's `path` param, same convention as node.image_folder's `folder`. mesh_index=-1 means whole scene, world-combined under the default scene's transform hierarchy (the default — \"just drop a model in\"); mesh_index >= 0 plus primitive_index select a single mesh or primitive in LOCAL space so the importer can place it via node.render_scene's per-object pos_*/rot_*/scale_* transforms instead of baking a node transform in. max_capacity is the pre-allocation ceiling in vertices; the glTF importer sets it to the exact parsed vertex count, so manual drops of meshes exceeding it are truncated with a logged warning rather than silently dropping the tail.",
    examples: [],
    picker: { label: "glTF Mesh", category: Atom },
    summary: "Loads a glTF/.glb model file from disk as mesh geometry, so imported 3D assets flow into the render pipeline like any other shape primitive.",
    category: Geometry3D,
    role: Source,
    aliases: ["gltf", "glb", "import mesh", "load model", "File In SOP"],
    extra_fields: {
        // (path, mesh_index, primitive_index) last parsed (or in flight).
        // Any change re-triggers a background parse.
        last_key: (String, i32, i32) = (String::new(), i32::MIN, i32::MIN),
        // Last successfully parsed geometry (CPU-side). Stays resident
        // across frames — only re-uploaded to `staging` when it changes.
        cached_verts: Vec<MeshVertex> = Vec::new(),
        // Shared-memory buffer holding `cached_verts`' bytes, copied into
        // the output buffer every frame via a blit.
        staging: Option<manifold_gpu::GpuBuffer> = None,
        staging_len_bytes: u64 = 0,
        // Background loader channel. `Some` means a parse is in flight;
        // we don't spawn another until it returns.
        pending_load: Option<mpsc::Receiver<Result<Vec<MeshVertex>, String>>> = None,
        // Whether `staging` currently reflects `cached_verts`.
        uploaded: bool = false,
    },
}

impl Primitive for GltfMeshSource {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // 1. Params.
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let mesh_index = match ctx.params.get("mesh_index") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => -1,
        };
        let primitive_index = match ctx.params.get("primitive_index") {
            Some(ParamValue::Float(n)) => n.round() as i32,
            _ => -1,
        };

        // 2. Re-trigger a background parse if the effective selection
        // changed since the last one we started.
        let key = (path.clone(), mesh_index, primitive_index);
        if key != self.last_key && self.pending_load.is_none() {
            self.last_key = key;
            self.cached_verts.clear();
            self.staging = None;
            self.staging_len_bytes = 0;
            self.uploaded = false;
            if !path.is_empty() {
                let selector = if mesh_index < 0 {
                    GltfMeshSelector::WholeScene
                } else if primitive_index < 0 {
                    GltfMeshSelector::Mesh {
                        mesh_index: mesh_index as u32,
                    }
                } else {
                    GltfMeshSelector::Primitive {
                        mesh_index: mesh_index as u32,
                        primitive_index: primitive_index as u32,
                    }
                };
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(load_gltf_mesh(&path_buf, selector));
                });
                self.pending_load = Some(rx);
            }
        }

        // 3. Drain any completed background parse.
        if self.pending_load.is_some() {
            let rx = self.pending_load.take().unwrap();
            match rx.try_recv() {
                Ok(Ok(verts)) => {
                    self.cached_verts = verts;
                    self.uploaded = false;
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_mesh_source: {e}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still in flight — put the receiver back.
                    self.pending_load = Some(rx);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_mesh_source: background load channel disconnected");
                }
            }
        }

        // 4. Output buffer + capacity.
        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let capacity = dst.size / std::mem::size_of::<MeshVertex>() as u64;

        if self.cached_verts.is_empty() {
            // Nothing parsed yet (or the path is empty / failed) — leave
            // the pre-bound buffer's existing contents; downstream nodes
            // see whatever they last saw (or zeros on first run).
            return;
        }

        // 5. (Re)build the staging buffer when cached_verts changed.
        if !self.uploaded {
            let n = self.cached_verts.len().min(capacity as usize);
            if self.cached_verts.len() > capacity as usize {
                log::warn!(
                    "node.gltf_mesh_source: mesh has {} vertices, capacity {} — truncating",
                    self.cached_verts.len(),
                    capacity
                );
            }
            let bytes = &self.cached_verts[..n];
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

        // 6. Copy staging → dst every frame (cheap blit; dst is the
        // chain-allocated buffer downstream nodes read from).
        if let Some(staging) = &self.staging {
            let copy_size = self.staging_len_bytes.min(dst.size);
            if copy_size > 0 {
                ctx.gpu_encoder()
                    .native_enc
                    .copy_buffer_to_buffer(staging, dst, copy_size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;
    use crate::node_graph::ports::{ArrayType, PortType};

    #[test]
    fn gltf_mesh_source_declares_zero_inputs_and_mesh_array_output() {
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(GltfMeshSource::TYPE_ID, "node.gltf_mesh_source");
        assert!(GltfMeshSource::INPUTS.is_empty());
        assert_eq!(GltfMeshSource::OUTPUTS.len(), 1);
        assert_eq!(GltfMeshSource::OUTPUTS[0].name, "vertices");
        assert_eq!(GltfMeshSource::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn gltf_mesh_source_param_names_in_order() {
        let names: Vec<&str> = GltfMeshSource::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["path", "mesh_index", "primitive_index", "max_capacity"]);
    }

    #[test]
    fn primitive_registers() {
        let prim = GltfMeshSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_mesh_source");
    }
}
