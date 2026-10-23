//! `node.mesh_fragment` — one piece of a shape broken into pieces (Shatter),
//! as the triangles the piece draws and the `MeshSource` its body collides
//! with.
//!
//! An imported mesh breaks inside `node.gltf_mesh_source` (its fragment
//! params). A built-in shape or a curve solid is made on the GPU, where a
//! piece cannot be cut, but each has a CPU mirror (`MeshSource`). This node
//! reads that mirror, splits a shape with few triangles until every piece
//! has some (`fragment_detail`), cuts piece `fragment_index` of
//! `fragment_count` with the partition an imported mesh uses, and writes it
//! to `vertices`. Its `source` is `MeshSource::Fragment`, whose CPU mirror is
//! the same piece, so the body's hull matches what draws.
//!
//! CPU-only, like `node.curve_solid_source`: the piece is cut once per change
//! of the shape and written to shared memory; a still shape writes nothing.

use std::borrow::Cow;
use std::sync::Arc;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::mesh_source::{MeshSource, fragment_detail};

const VERTEX_BYTES: u64 = std::mem::size_of::<MeshVertex>() as u64;

manifold_node_engine::primitive! {
    name: MeshFragment,
    type_id: "node.mesh_fragment",
    purpose: "Cut piece `fragment_index` of `fragment_count` from the shape `shape` describes: its triangles (each split in four until every piece has at least four of the shape's), partitioned as an imported mesh's Shatter pieces are. `vertices` is the piece; `source` is MeshSource::Fragment, whose CPU mirror is the same piece, for the piece's rigid body. `in` is the drawn shape the piece replaces: only its capacity is read, to size `vertices`.",
    inputs: {
        shape: MeshSource required,
        in: Array(MeshVertex) required,
    },
    outputs: {
        vertices: Array(MeshVertex),
        source: MeshSource,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("fragment_count"),
            label: "Pieces",
            ty: ParamType::Int,
            default: ParamValue::Float(2.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fragment_index"),
            label: "Piece",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Shatter's load pass wires it: the shape's MeshSource (a built-in mesh's or node.curve_solid_source's `source`) into `shape`, the drawn vertices into `in`, `vertices` to the piece's scene object and `source` to the piece's rigid body. An imported mesh breaks in node.gltf_mesh_source instead.",
    examples: [],
    picker: { label: "Mesh Fragment", category: Atom },
    summary: "One piece of a shape broken into pieces: the triangles it draws and the shape its body collides with.",
    category: Geometry3D,
    role: Filter,
    aliases: ["mesh fragment", "shatter piece", "fragment"],
    boundary_reason: NonGpu,
    extra_fields: {
        // The shape and cut last written, and the buffer they went to.
        written: Option<(MeshSource, u32, u32, usize)> = None,
        published: Option<MeshSource> = None,
    },
}

fn int(params: &ParamValues, name: &str, default: u32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) if v.is_finite() => v.round().max(0.0) as u32,
        _ => default,
    }
}

/// The splits a piece of a shape drawn with `capacity` vertices is cut
/// after. Read from the capacity, so the output size and the cut agree.
fn detail(capacity: u64, count: u32) -> u32 {
    fragment_detail(capacity / 3, count)
}

impl Primitive for MeshFragment {
    /// The drawn shape's capacity, times four per split: a piece is never
    /// more triangles than the split whole.
    fn array_output_capacity(&self, port_name: &str, params: &ParamValues, input_capacities: &[(&str, u32)]) -> Option<u32> {
        (port_name == "vertices").then(|| {
            let capacity = input_capacities.iter().find(|(port, _)| *port == "in").map_or(3, |&(_, n)| u64::from(n).max(3));
            let grown = capacity << (2 * detail(capacity, int(params, "fragment_count", 2).max(1)));
            grown.min(u64::from(u32::MAX)) as u32
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let count = int(ctx.params, "fragment_count", 2).max(1);
        let index = int(ctx.params, "fragment_index", 0);
        let (Some(whole), Some(drawn), Some(out)) = (ctx.inputs.mesh_source("shape"), ctx.inputs.array("in"), ctx.outputs.array("vertices"))
        else {
            return;
        };
        let destination = out.identity_key();
        let same = self.written.as_ref().is_some_and(|(source, c, i, at)| *source == whole && *c == count && *i == index && *at == destination);
        if !same || !ctx.outputs_retained() {
            let piece = MeshSource::Fragment { of: Arc::new(whole.clone()), count, index, detail: detail(drawn.size / VERTEX_BYTES, count) };
            let vertices = match piece.load_vertices() {
                Ok(vertices) => vertices,
                Err(error) => {
                    ctx.error(format!("Mesh Fragment: {error}"));
                    Vec::new()
                }
            };
            let capacity = (out.size / VERTEX_BYTES) as usize;
            let kept = vertices.len().min(capacity) / 3 * 3;
            // Safety: shared-memory buffer pre-bound by the chain build,
            // writes clamped to its capacity; the executor runs one node at a
            // time on the content thread. The tail is zeroed so it draws
            // nothing.
            unsafe {
                out.write(0, bytemuck::cast_slice(&vertices[..kept]));
                let tail = vec![0u8; (capacity - kept) * VERTEX_BYTES as usize];
                out.write(kept as u64 * VERTEX_BYTES, &tail);
            }
            self.written = Some((whole, count, index, destination));
            self.published = Some(piece);
        } else {
            ctx.mark_output_content_unchanged();
        }
        if let Some(piece) = &self.published {
            ctx.outputs.set_mesh_source("source", piece.clone());
        }
    }
}
