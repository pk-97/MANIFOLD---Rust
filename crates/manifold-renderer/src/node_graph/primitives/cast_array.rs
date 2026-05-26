//! `node.cast_as_*` — relabel an `Array<Anonymous>` wire (raw byte
//! buffer from `node.wgsl_compute`) as a typed `Array<T>` wire so the
//! curated typed pipeline downstream sees its expected type.
//!
//! Each cast is a wire-label transformation only. At chain build the
//! output's resource id is aliased to the input's slot — same
//! physical GpuBuffer, just a different `ItemKind` label on the
//! downstream wire. Runtime `run()` is a no-op; the cast atoms exist
//! to satisfy the wire validator's type discipline at the boundary
//! between `wgsl_compute`'s generic byte output and the typed
//! consumer.
//!
//! Wire validator relaxation: an `Array(Anonymous, size, align)`
//! matches `Array(KnownKind, same_size, same_align)` in either
//! direction (see `validation::port_types_compatible`). That's what
//! lets `wgsl_compute(Anonymous-64) → cast_as_particle → scatter_particles`
//! validate cleanly: wgsl_compute's output (Anonymous-64) matches the
//! cast atom's input (Anonymous-64) exactly, the cast atom's output
//! (Particle-64) matches scatter's input (Particle-64) exactly, and
//! the chain build aliases all three to the same physical buffer.
//!
//! Each cast is its own atom rather than one configurable primitive
//! because the `primitive!` macro requires static port types; mux-style
//! enums over output types would need dynamic port shape. Adding a new
//! typed buffer (custom user vertex format) is one new ~30-line block
//! in this file.

use crate::generators::compute_common::Particle;
use crate::generators::mesh_common::{CurvePoint, EdgePair, InstanceTransform, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

// ── Anonymous-blob marker types ─────────────────────────────────
//
// These are byte-blob structs sized to match common typed buffers.
// They exist only to give the `ArrayAnonymous(T)` macro a Pod type
// with the right size + align — the resulting wire is Anonymous, not
// typed. Adding a new size means adding a `BlobN` here.

#[repr(C, align(4))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Blob4([u8; 4]);

#[repr(C, align(4))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Blob8([u8; 8]);

#[repr(C, align(4))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Blob32([u8; 32]);

#[repr(C, align(4))]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Blob64([u8; 64]);

// ── cast_as_particle (64 bytes) ───────────────────────────────────

crate::primitive! {
    name: CastAsParticle,
    type_id: "node.cast_as_particle",
    purpose: "Relabel an Array<Anonymous, item_size=64> wire (typical wgsl_compute output writing `array<Particle>` storage) as Array<Particle> so the typed particle pipeline (scatter_particles, array_diffuse_particles, etc.) accepts it. Aliased in/out — same physical buffer, just a wire-label transformation. Runtime no-op.",
    inputs: {
        in: ArrayAnonymous(Blob64) required,
    },
    outputs: {
        out: Array(Particle),
    },
    params: [],
    composition_notes: "Pair with `node.wgsl_compute` writing `var<storage, read_write> particles: array<Particle>` upstream; downstream typed consumers (scatter_particles, array_diffuse_particles, integrate_particles, array_feedback) see Array<Particle>. The cast is wire metadata only — no GPU dispatch, no buffer copy.",
    examples: [],
    picker: { label: "Cast as Particle", category: Atom },
}

impl Primitive for CastAsParticle {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

// ── cast_as_u32 (4 bytes) ─────────────────────────────────────────

crate::primitive! {
    name: CastAsU32,
    type_id: "node.cast_as_u32",
    purpose: "Relabel an Array<Anonymous, item_size=4> wire (typical wgsl_compute output writing `array<atomic<u32>>` storage) as Array<u32> so resolve_accumulator (and other u32-grid consumers) accepts it. Aliased in/out, runtime no-op.",
    inputs: {
        in: ArrayAnonymous(Blob4) required,
    },
    outputs: {
        out: Array(u32),
    },
    params: [],
    composition_notes: "Pair with `node.wgsl_compute` writing `var<storage, read_write> accum: array<atomic<u32>>` (the polar splat / scatter pattern) upstream; downstream `node.resolve_accumulator` lifts the u32 grid into a float texture. Cast is wire metadata only — no GPU dispatch.",
    examples: [],
    picker: { label: "Cast as U32", category: Atom },
}

impl Primitive for CastAsU32 {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

// ── cast_as_mesh_vertex (32 bytes) ────────────────────────────────

crate::primitive! {
    name: CastAsMeshVertex,
    type_id: "node.cast_as_mesh_vertex",
    purpose: "Relabel an Array<Anonymous, item_size=32> wire (wgsl_compute writing a 32-byte vertex+normal struct) as Array<MeshVertex> for the 3D mesh pipeline (rotate_3d, project_3d, render_3d_mesh, render_lines with edges). Aliased in/out, runtime no-op.",
    inputs: {
        in: ArrayAnonymous(Blob32) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [],
    composition_notes: "Bridges a `node.wgsl_compute` writing a mesh-vertex-shaped struct (position + normal, 32 bytes) into the 3D wireframe / mesh-render pipeline. Use case: an open-family vertex generator (wgsl_compute with switch on shape variant) that wants to feed `rotate_3d → project_3d → render_lines` without registering a new Rust primitive per variant.",
    examples: [],
    picker: { label: "Cast as Mesh Vertex", category: Atom },
}

impl Primitive for CastAsMeshVertex {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

// ── cast_as_curve_point (8 bytes) ─────────────────────────────────

crate::primitive! {
    name: CastAsCurvePoint,
    type_id: "node.cast_as_curve_point",
    purpose: "Relabel an Array<Anonymous, item_size=8> wire (wgsl_compute writing a 2D-point struct) as Array<CurvePoint> for the line-render pipeline (render_lines). Aliased in/out, runtime no-op.",
    inputs: {
        in: ArrayAnonymous(Blob8) required,
    },
    outputs: {
        out: Array(CurvePoint),
    },
    params: [],
    composition_notes: "Bridges a `node.wgsl_compute` writing 2D curve points (8-byte vec2 in origin-centered pre-aspect curve space) into `node.render_lines`. Use case: open-family parametric curve generators (custom xy-curve formulas as user-authored WGSL) that want to feed render_lines without per-curve Rust primitives.",
    examples: [],
    picker: { label: "Cast as Curve Point", category: Atom },
}

impl Primitive for CastAsCurvePoint {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

// ── cast_as_edge_pair (8 bytes) ───────────────────────────────────

crate::primitive! {
    name: CastAsEdgePair,
    type_id: "node.cast_as_edge_pair",
    purpose: "Relabel an Array<Anonymous, item_size=8> wire (wgsl_compute writing an edge-pair struct) as Array<EdgePair> for line-render topology (render_lines' `edges` input). Aliased in/out, runtime no-op.",
    inputs: {
        in: ArrayAnonymous(Blob8) required,
    },
    outputs: {
        out: Array(EdgePair),
    },
    params: [],
    composition_notes: "Bridges a `node.wgsl_compute` writing edge pairs (2× u32 vertex indices, 8 bytes) into `node.render_lines`'s edges input port. Use case: open-family topology generators that produce dynamic vertex connectivity (procedural meshes, lattice graphs) without per-topology Rust primitives.",
    examples: [],
    picker: { label: "Cast as Edge Pair", category: Atom },
}

impl Primitive for CastAsEdgePair {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

// ── cast_as_instance_transform (32 bytes) ─────────────────────────

crate::primitive! {
    name: CastAsInstanceTransform,
    type_id: "node.cast_as_instance_transform",
    purpose: "Relabel an Array<Anonymous, item_size=32> wire as Array<InstanceTransform> for the instanced mesh render pipeline. Aliased in/out, runtime no-op.",
    inputs: {
        in: ArrayAnonymous(Blob32) required,
    },
    outputs: {
        out: Array(InstanceTransform),
    },
    params: [],
    composition_notes: "Bridges a `node.wgsl_compute` writing per-instance transforms (32-byte struct) into `node.render_instanced_3d_mesh`. Use case: open-family instance-layout generators (procedural placement, force-directed layouts, audio-reactive scattering) without per-layout Rust primitives.",
    examples: [],
    picker: { label: "Cast as Instance Transform", category: Atom },
}

impl Primitive for CastAsInstanceTransform {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("in", "out")]
    }

    fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn cast_as_particle_declares_aliased_anonymous_in_typed_out() {
        use crate::node_graph::ports::{ArrayType, ItemKind, PortType};
        assert_eq!(CastAsParticle::TYPE_ID, "node.cast_as_particle");
        let in_port = CastAsParticle::INPUTS.iter().find(|p| p.name == "in").unwrap();
        match in_port.ty {
            PortType::Array(a) => {
                assert_eq!(a.item_kind, ItemKind::Anonymous);
                assert_eq!(a.item_size, 64);
            }
            _ => panic!("expected Array input"),
        }
        let out_port = CastAsParticle::OUTPUTS.iter().find(|p| p.name == "out").unwrap();
        assert_eq!(out_port.ty, PortType::Array(ArrayType::of_known::<Particle>()));
        let prim = CastAsParticle::new();
        assert_eq!(Primitive::aliased_array_io(&prim), &[("in", "out")]);
    }

    #[test]
    fn every_cast_atom_registers_with_unique_type_id() {
        let p: &dyn EffectNode = &CastAsParticle::new();
        assert_eq!(p.type_id().as_str(), "node.cast_as_particle");
        let p: &dyn EffectNode = &CastAsU32::new();
        assert_eq!(p.type_id().as_str(), "node.cast_as_u32");
        let p: &dyn EffectNode = &CastAsMeshVertex::new();
        assert_eq!(p.type_id().as_str(), "node.cast_as_mesh_vertex");
        let p: &dyn EffectNode = &CastAsCurvePoint::new();
        assert_eq!(p.type_id().as_str(), "node.cast_as_curve_point");
        let p: &dyn EffectNode = &CastAsEdgePair::new();
        assert_eq!(p.type_id().as_str(), "node.cast_as_edge_pair");
        let p: &dyn EffectNode = &CastAsInstanceTransform::new();
        assert_eq!(p.type_id().as_str(), "node.cast_as_instance_transform");
    }
}
