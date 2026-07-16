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
use crate::node_graph::gltf_load::load_gltf_morph_deltas;
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
            range: Some((0.0, 1024.0)),
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
            if !path.is_empty() && material_index >= 0 {
                let path_buf = std::path::PathBuf::from(&path);
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let result = load_gltf_morph_deltas(&path_buf, material_index as u32);
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

        if let Some(staging) = &self.staging {
            let copy_size = self.staging_len_bytes.min(dst.size);
            if copy_size > 0 {
                ctx.gpu_encoder().native_enc.copy_buffer_to_buffer(staging, dst, copy_size);
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
