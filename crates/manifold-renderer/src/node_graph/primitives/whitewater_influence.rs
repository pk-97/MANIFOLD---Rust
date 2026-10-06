//! FLIP InfluenceGrid decay and source application (spread is disabled in the engine).
//! Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use super::{sort_particles_into_cells::float_param, whitewater_obstacle_source::WhitewaterSource};
use crate::node_graph::{
    effect_node::{EffectNodeContext, ParamValues},
    parameters::{ParamDef, ParamType, ParamValue},
    primitive::Primitive,
};
use std::borrow::Cow;

crate::primitive! {
    name: WhitewaterInfluence,
    type_id: "node.whitewater_influence",
    purpose: "Move every influence node toward Base Level by Decay Rate times dt, then replace nodes within three cells of a solid with its nearest obstacle influence; domain sources use Base Level. Reset starts at Base Level. Matches FLIP InfluenceGrid with its default disabled spread.",
    inputs: {
        values: Array(f32) required, solid: Array(f32) required, source: Array(WhitewaterSource) required,
        base_level: ScalarF32 optional, decay_rate: ScalarF32 optional, dt: ScalarF32 optional,
        cell_size: ScalarF32 optional, reset: ScalarF32 optional, source_present: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("base_level", "Base Influence", 1.0, 0.0, 100.0),
        float_param!("decay_rate", "Influence Decay", 2.0, 0.0, 100.0),
        float_param!("dt", "Tick Seconds", 1.0 / 60.0, 0.0, 1.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
        float_param!("reset", "Reset", 0.0, 0.0, 1.0),
        float_param!("source_present", "Source Present", 1.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Carry out into values on the next liquid tick. Source is whitewater_obstacle_source or equivalent nearest-object metadata. Sample by emitter cell index, without interpolation, when counting emissions.",
    examples: [],
    summary: "Decays obstacle influence and reapplies solid sources.", category: Particles3D, role: Filter,
    aliases: ["obstacle influence", "whitewater influence"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/whitewater_influence_body.wgsl"),
    input_access: [Coincident, Coincident, Coincident],
    output_capacity: crate::node_graph::freeze::classify::FusedOutputCapacity::FromInput { input: "values" },
}
impl Primitive for WhitewaterInfluence {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| inputs.iter().find(|(p, _)| *p == "values").map(|(_, n)| *n))
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        super::whitewater_emitter_dispatch::run::<Self>(
            ctx,
            &mut self.pipeline,
            &["values", "solid", "source"],
            4,
            4,
        );
    }
}
