//! `node.remap_cut_weights` — interpolate per-vertex scalar weights through a
//! triangle/barycentric cut map so Math View and geometry masks keep the same
//! logical layout as remapped mesh/reference data.

use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: RemapCutWeights,
    type_id: "node.remap_cut_weights",
    purpose: "Interpolate an Array<f32> of source per-vertex weights through an Array<Vec4Vertex> cut map. Map x/y/z are barycentrics and map w is the exact source triangle index; invalid or padded map entries emit zero.",
    inputs: {
        in: Array(f32) required,
        map: Array(Vec4Vertex) required,
    },
    outputs: { out: Array(f32), },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Use the same cut map as node.remap_mesh_cut for incoming, reference, and Math View weights. Source weights must have the reference mesh's triangle-list layout; padding is explicitly zero.",
    examples: [],
    picker: { label: "Remap Cut Weights", category: Atom },
    summary: "Carries per-vertex masks through a triangle cut map.",
    category: Geometry3D,
    role: Filter,
    aliases: ["remap weights", "cut weights"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/remap_cut_weights_body.wgsl"),
    input_access: [BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "map" },
    wgsl_includes: [include_str!("shaders/mesh_cut_map_valid.wgsl")],
    extra_fields: { last_key: Option<[u64; 7]> = None },
}

impl Primitive for RemapCutWeights {
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

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        super::mesh_cut_remap::run::<Self>(ctx, &mut self.pipeline, &mut self.last_key, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{ArrayType, PortType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_scalar_and_map_inputs_and_map_sized_output() {
        assert_eq!(RemapCutWeights::TYPE_ID, "node.remap_cut_weights");
        assert_eq!(
            RemapCutWeights::INPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
        assert_eq!(
            RemapCutWeights::INPUTS[1].ty,
            PortType::Array(ArrayType::of_known::<Vec4Vertex>())
        );
        assert_eq!(
            RemapCutWeights::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<f32>())
        );
        let p = RemapCutWeights::new();
        assert_eq!(
            Primitive::array_output_capacity(&p, "out", &Default::default(), &[("map", 17)]),
            Some(17)
        );
    }
}
