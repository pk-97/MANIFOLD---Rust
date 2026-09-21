//! `node.remap_mesh_cut` — gather a mesh through a triangle/barycentric cut map.
//!
//! The map is deliberately a separate buffer so current and original reference
//! geometry can be remapped by the same map before a fragment stage runs.

use std::borrow::Cow;

use crate::generators::mesh_common::{MeshVertex, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: RemapMeshCut,
    type_id: "node.remap_mesh_cut",
    purpose: "Gather an Array<MeshVertex> through an Array<Vec4Vertex> cut map. Each map entry stores barycentric x/y/z and an exact source triangle index in w; invalid entries with w < 0 produce finite zero-area geometry. Positions, UVs, normals, and tangents are interpolated and the shading frame is orthonormalized.",
    inputs: {
        in: Array(MeshVertex) required,
        map: Array(Vec4Vertex) required,
    },
    outputs: { out: Array(MeshVertex), },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Use one shared map for both a fragment stage's incoming mesh and its aligned original reference. The map is compact and deterministic; its capacity is the downstream vertex capacity. Exact source-corner maps preserve the source attributes and GPU frame values remain finite for invalid padding.",
    examples: [],
    picker: { label: "Remap Mesh Cut", category: Atom },
    summary: "Applies a triangle and barycentric cut map to a mesh while preserving its shading frame.",
    category: Geometry3D,
    role: Filter,
    aliases: ["remap cut", "cut map", "mesh remap"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/remap_mesh_cut_body.wgsl"),
    input_access: [BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "map" },
    wgsl_includes: [include_str!("shaders/mesh_cut_map_valid.wgsl")],
    extra_fields: { last_key: Option<[u64; 5]> = None },
}

impl Primitive for RemapMeshCut {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "map")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }

    /// Mesh revision declaration (SCENE_MODIFIER_RT_DESIGN.md §3.1):
    /// output record `idx` is a barycentric gather of the source
    /// triangle named by the coincident map record's `w`
    /// (`shaders/remap_mesh_cut_body.wgsl`), and output capacity follows
    /// the `map` input (see `array_output_capacity` above) — so topology
    /// depends on the source's topology plus the map's content, and
    /// positions are Written.
    fn mesh_output_rule(&self, port: &str) -> crate::node_graph::mesh_change::MeshOutputRule<'_> {
        use crate::node_graph::mesh_change::{
            MeshAspect, MeshDependency, MeshOutputRule, MeshRevisionRule,
        };
        if port == "out" {
            return MeshOutputRule {
                topology: MeshRevisionRule::Dependencies(&[
                    MeshDependency {
                        input: Cow::Borrowed("in"),
                        aspect: MeshAspect::Topology,
                    },
                    MeshDependency {
                        input: Cow::Borrowed("map"),
                        aspect: MeshAspect::Content,
                    },
                ]),
                positions: MeshRevisionRule::Written,
            };
        }
        MeshOutputRule {
            topology: MeshRevisionRule::Written,
            positions: MeshRevisionRule::Written,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        super::mesh_cut_remap::run::<Self>(ctx, &mut self.pipeline, &mut self.last_key, 64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{ArrayType, PortType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_mesh_and_map_inputs_and_map_sized_output() {
        assert_eq!(RemapMeshCut::TYPE_ID, "node.remap_mesh_cut");
        assert_eq!(
            RemapMeshCut::INPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<MeshVertex>())
        );
        assert_eq!(
            RemapMeshCut::INPUTS[1].ty,
            PortType::Array(ArrayType::of_known::<Vec4Vertex>())
        );
        assert_eq!(
            RemapMeshCut::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<MeshVertex>())
        );
        let p = RemapMeshCut::new();
        assert_eq!(
            Primitive::array_output_capacity(&p, "out", &Default::default(), &[("map", 99)]),
            Some(99)
        );
    }
}
