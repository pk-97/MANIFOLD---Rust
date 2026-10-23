//! `node.arrange_copies` — lay copies out exactly as a copies body does.
//!
//! The rest places come from `manifold_core::copy_layout`, the formula the
//! physics world rests a copies body's copies by, so Copy Blend moves copy
//! `i` to the slot copy `i` would take if the copies' own Layout were
//! switched, and no two blend paths cross. Computed on the CPU only when an
//! input changes, then uploaded as an immutable snapshot.

use std::borrow::Cow;

use manifold_core::copies_body::{COPY_LAYOUT_LABELS, SIZE_SPACING_DEFAULT, SIZE_SPACING_RANGE};
use manifold_core::copy_layout::{Arrangement, CopyLayout};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::exec::instance_upload::InstanceSnapshotUpload;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::transform::quat_to_render_scene_euler;

/// A copies body's Columns default, for a node no copies body feeds.
const COLUMNS_DEFAULT: f32 = 16.0;

manifold_node_engine::primitive! {
    name: GenerateInstanceTransforms,
    type_id: "node.arrange_copies",
    purpose: "Emit an Array<InstanceTransform> of active_count copies laid out exactly as a copies body lays out its own (manifold_core::copy_layout): Grid, Scatter, Line, Ring, Spiral or Random, spaced in multiples of one copy's box width (copy_box) by copy_grid's Spacing, Columns a row or ring, each copy turned by copy_turn, the whole layout placed and turned by copy_frame. Line and Grid grow one way from the first copy on the origin; Ring spreads its copies evenly round the full circle, up to Columns a ring. Each copy's scale is copy_frame's X scale times base_scale (Copy Size). Slots past active_count are zero.",
    inputs: {
        copy_frame: Transform optional,
        copy_box: ScalarVec3 optional,
        copy_turn: ScalarVec3 optional,
        copy_grid: ScalarVec2 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(65_536.0),
            range: Some((1.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(64.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("layout"),
            label: "Layout",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: &COPY_LAYOUT_LABELS,
        },
        ParamDef { name: Cow::Borrowed("columns"), label: "Columns", ty: ParamType::Int, default: ParamValue::Float(COLUMNS_DEFAULT), range: Some((1.0, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("spacing"), label: "Spacing", ty: ParamType::Float, default: ParamValue::Float(SIZE_SPACING_DEFAULT), range: Some(SIZE_SPACING_RANGE), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("box_x"), label: "Copy Width", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("box_y"), label: "Copy Height", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("box_z"), label: "Copy Depth", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.0, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("turn_x"), label: "Copy Turn X", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("turn_y"), label: "Copy Turn Y", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("turn_z"), label: "Copy Turn Z", ty: ParamType::Angle, default: ParamValue::Float(0.0), range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)), enum_values: &[] },
        ParamDef {
            name: Cow::Borrowed("base_scale"),
            label: "Copy Size",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire a copies body's copy_frame, copy_box, copy_turn, copy_grid and active_count (node.rigid_body outputs; Copy Blend's stage takes them from the copies context) to lay a second arrangement of the same copies, then node.blend_copies between the two. Each wired input replaces its params: copy_box the box_x/y/z copy width, copy_turn turn_x/y/z, copy_grid (Columns, Spacing) columns and spacing, active_count its param. An unwired copy_frame is the identity, so one copy sits on the origin. max_capacity is the allocation ceiling (max_capacity × 32 bytes). The places are recomputed only when an input changes.",
    examples: [],
    picker: { label: "Arrange Copies", category: Atom },
    summary: "Lays copies out the way a copies body does, in a grid, line, ring, spiral, pile or random spread, giving each one a position to render at.",
    category: Geometry3D,
    role: Source,
    aliases: ["arrange copies", "generate instance transforms", "instance layout", "scatter", "place"],
    boundary_reason: IoBridge,
    extra_fields: {
        // The places last computed, and what they were computed from.
        places: Vec<InstanceTransform> = Vec::new(),
        laid: Option<(Arrangement, [u32; 3], u32)> = None,
        version: u64 = 0,
        upload: InstanceSnapshotUpload = InstanceSnapshotUpload::default(),
    },
}

fn vector<const N: usize>(value: Option<ParamValue>, default: [f32; N]) -> [f32; N] {
    match (value, N) {
        (Some(ParamValue::Vec2(v)), 2) => std::array::from_fn(|i| v[i]),
        (Some(ParamValue::Vec3(v)), 3) => std::array::from_fn(|i| v[i]),
        _ => default,
    }
}

impl Primitive for GenerateInstanceTransforms {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(out) = ctx.outputs.array("instances") else {
            return;
        };
        let capacity = (out.size / std::mem::size_of::<InstanceTransform>() as u64) as usize;
        let count = (ctx.scalar_or_param("active_count", 64.0).round().max(0.0) as usize).min(capacity);
        let layout = match ctx.params.get("layout") {
            Some(ParamValue::Enum(value)) => *value as f32,
            Some(ParamValue::Float(value)) => *value,
            _ => 0.0,
        };
        let frame = ctx.inputs.transform("copy_frame").unwrap_or_default();
        let width = vector(ctx.inputs.scalar("copy_box"), ["box_x", "box_y", "box_z"].map(|name| ctx.param_f32(name, 1.0)));
        let turn = vector(ctx.inputs.scalar("copy_turn"), ["turn_x", "turn_y", "turn_z"].map(|name| ctx.param_f32(name, 0.0)));
        let [columns, spacing] = vector(
            ctx.inputs.scalar("copy_grid"),
            [ctx.param_f32("columns", COLUMNS_DEFAULT), ctx.param_f32("spacing", SIZE_SPACING_DEFAULT)],
        );
        let arrangement = Arrangement {
            layout: CopyLayout::from_value(layout),
            count,
            columns: columns.round().clamp(1.0, 64.0) as usize,
            spacing,
            turn,
            origin: frame.pos,
            rotation: frame.rot_euler,
        };
        let scale = frame.scale[0] * ctx.param_f32("base_scale", 1.0);
        let key = (arrangement, width.map(f32::to_bits), scale.to_bits());
        if self.laid != Some(key) {
            self.places.clear();
            self.places.extend((0..count).map(|index| place(&arrangement, width, scale, index)));
            self.laid = Some(key);
            self.version = self.version.wrapping_add(1);
        }
        let retained = ctx.outputs_retained();
        let Some(gpu) = ctx.gpu.as_deref_mut() else {
            return;
        };
        match self.upload.upload(gpu, out, &self.places, self.version, retained) {
            Ok(true) => {}
            Ok(false) => ctx.mark_output_content_unchanged(),
            Err(error) => ctx.error(error),
        }
    }
}

/// Copy `index`'s instance: its rest place, as the physics world publishes
/// a resting copy.
fn place(arrangement: &Arrangement, width: [f32; 3], scale: f32, index: usize) -> InstanceTransform {
    let rest = arrangement.rest(width, index);
    let [x, y, z] = quat_to_render_scene_euler(rest.rotation);
    InstanceTransform {
        pos_scale: [rest.position[0], rest.position[1], rest.position[2], scale],
        rot_pad: [x, y, z, 0.0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_unwired_copy_is_the_identity_instance() {
        let arrangement = Arrangement {
            layout: CopyLayout::Grid,
            count: 1,
            columns: COLUMNS_DEFAULT as usize,
            spacing: SIZE_SPACING_DEFAULT,
            turn: [0.0; 3],
            origin: [0.0; 3],
            rotation: [0.0; 3],
        };
        let instance = place(&arrangement, [1.0; 3], 1.0, 0);
        assert_eq!(instance.pos_scale, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(instance.rot_pad, [0.0; 4]);
    }
}
