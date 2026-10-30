//! `node.matter_material` — one Matter item's material as a row of the
//! scene's material table (MATTER_SAND_DESIGN.md section 7). Every Matter
//! item in a scene runs on one solver; each item's node writes its row into a
//! copy of the table it is handed and folds its waves, damping and density
//! into the scalars the domain picks the step from. The last node's table and
//! scalars feed the solver.

use std::borrow::Cow;

use manifold_gpu::{BufferUse, GpuBuffer};

use crate::matter::{
    FRICTION_ANGLE_RANGE, MATERIAL_ROW_FLOATS, MATTER_KINDS, MAX_MATTER_SLOTS, MatterDials,
    MatterKind, MatterMaterial, MatterRule, SAND_DENSITY, SAND_FRICTION_ANGLE, SNOW_HARDENING,
};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

/// The Cohesion dial's range, kPa: 0 is dry sand, 20 holds a cliff about
/// ten metres tall.
const COHESION_KPA_RANGE: (f32, f32) = (0.0, 20.0);
/// The Density dial's range, kg/m³: snow and powder up to wet heavy soil.
const DENSITY_RANGE: (f32, f32) = (100.0, 3000.0);
/// Snow's Hardening ξ: 0 never stiffens as it packs; Stomakhin et al. 2013
/// ran 5 to 10.
const HARDENING_RANGE: (f32, f32) = (0.0, 20.0);
/// The shear Yield Stress, Pa: a runny goo up to a firm clay.
const YIELD_STRESS_RANGE: (f32, f32) = (0.0, 5.0e4);
/// The plastic Viscosity, Pa·s: water-thin up to tar.
const VISCOSITY_RANGE: (f32, f32) = (0.0, 1.0e4);

/// Bytes of one row and of the whole table.
const ROW_BYTES: u64 = (MATERIAL_ROW_FLOATS * 4) as u64;
const TABLE_BYTES: u64 = ROW_BYTES * MAX_MATTER_SLOTS as u64;

manifold_node_engine::primitive! {
    name: MatterMaterialRow,
    type_id: "node.matter_material",
    purpose: "Make one Matter item's material, from its dials, into its slot's row of the scene's material table: the Lamé moduli, friction, cohesion, rule, yield strain, relax time, hardening, density, the water's lift and drag on it and its wall friction. Copies the table it is handed (materials_in) and writes its row over it; folds the fastest pressure wave as built (wave), snow's (snow_wave), the fastest damping diffusivity η/ρ (diffusivity) and the heaviest density into the ones handed in, and passes the first item's wall friction on.",
    inputs: {
        materials_in: Array(f32) optional,
        wave_in: ScalarF32 optional,
        snow_wave_in: ScalarF32 optional,
        diffusivity_in: ScalarF32 optional,
        density_in: ScalarF32 optional,
        wall_friction_in: ScalarF32 optional,
    },
    outputs: {
        materials: Array(f32),
        wave: ScalarF32,
        snow_wave: ScalarF32,
        diffusivity: ScalarF32,
        density: ScalarF32,
        wall_friction: ScalarF32,
    },
    params: [
        ParamDef { name: Cow::Borrowed("slot"), label: "Slot", ty: ParamType::Int, default: ParamValue::Float(0.0), range: Some((0.0, (MAX_MATTER_SLOTS - 1) as f32)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("material"), label: "Material", ty: ParamType::Enum, default: ParamValue::Enum(0), range: Some((0.0, 5.0)), enum_values: &MATTER_KINDS },
        ParamDef { name: Cow::Borrowed("stiffness"), label: "Stiffness", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.5, 3.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("friction_angle"), label: "Friction Angle", ty: ParamType::Float, default: ParamValue::Float(SAND_FRICTION_ANGLE), range: Some(FRICTION_ANGLE_RANGE), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cohesion"), label: "Cohesion (kPa)", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some(COHESION_KPA_RANGE), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("density"), label: "Density (kg/m³)", ty: ParamType::Float, default: ParamValue::Float(SAND_DENSITY), range: Some(DENSITY_RANGE), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("hardening"), label: "Hardening", ty: ParamType::Float, default: ParamValue::Float(SNOW_HARDENING), range: Some(HARDENING_RANGE), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("yield_stress"), label: "Yield Stress (Pa)", ty: ParamType::Float, default: ParamValue::Float(1.0e4), range: Some(YIELD_STRESS_RANGE), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("viscosity"), label: "Viscosity (Pa·s)", ty: ParamType::Float, default: ParamValue::Float(20.0), range: Some(VISCOSITY_RANGE), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "One per Matter item, chained in slot order (slot 0 the item whose node.matter_domain runs the solver): each node's materials and scalars feed the next one's *_in inputs. The last node's materials feed node.matter_state, node.matter_to_grid, node.grid_to_matter and node.matter_stats, which read each point's row by the slot in its affine_z.w; its wave, snow_wave, diffusivity, density and wall_friction feed node.matter_domain, which picks the substeps from them. Rows past the chain's slots are zero.",
    examples: ["SandColumn"],
    picker: { label: "Matter Material", category: Atom },
    summary: "One Matter item's material, as a row of the table every grain reads its own from.",
    category: Particles3D,
    role: Source,
    aliases: ["matter material", "mpm material", "material table"],
    extra_fields: {
        // The row as last written and the shared buffer holding it: a new
        // buffer when the row changes, so a frame in flight keeps reading the
        // one it was encoded with.
        row: Option<[f32; MATERIAL_ROW_FLOATS]> = None,
        upload: Option<GpuBuffer> = None,
    },
}

impl Primitive for MatterMaterialRow {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "materials").then_some((MAX_MATTER_SLOTS * MATERIAL_ROW_FLOATS) as u32)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let ranged = |ctx: &EffectNodeContext<'_, '_>, name, default, (lo, hi): (f32, f32)| {
            f64::from(ctx.scalar_or_param(name, default).clamp(lo, hi))
        };
        let slot = (ctx.scalar_or_param("slot", 0.0).round().max(0.0) as usize)
            .min(MAX_MATTER_SLOTS - 1);
        let kind = match ctx.params.get("material") {
            Some(ParamValue::Enum(index)) => MatterKind::from_index(*index as usize),
            _ => None,
        }
        .unwrap_or(MatterKind::Sand);
        let density = ranged(ctx, "density", SAND_DENSITY, DENSITY_RANGE);
        let material = MatterMaterial::of(
            kind,
            MatterDials {
                stiffness: ranged(ctx, "stiffness", 1.0, (0.5, 3.0)),
                friction_angle: ranged(ctx, "friction_angle", SAND_FRICTION_ANGLE, FRICTION_ANGLE_RANGE),
                cohesion_kpa: ranged(ctx, "cohesion", 0.0, COHESION_KPA_RANGE),
                hardening: ranged(ctx, "hardening", SNOW_HARDENING, HARDENING_RANGE),
                yield_stress: ranged(ctx, "yield_stress", 1.0e4, YIELD_STRESS_RANGE),
                viscosity: ranged(ctx, "viscosity", 20.0, VISCOSITY_RANGE),
                density,
            },
        );
        let wave = material.wave_speed(density) as f32;
        let snow = material.rule == MatterRule::Snow;
        // Unwired, a fold starts from nothing: the first item's own.
        let folded = |ctx: &EffectNodeContext<'_, '_>, name: &str, own: f32| {
            ctx.scalar_or_param(name, 0.0).max(own)
        };
        let wave_out = folded(ctx, "wave_in", if snow { 0.0 } else { wave });
        let snow_wave_out = folded(ctx, "snow_wave_in", if snow { wave } else { 0.0 });
        let diffusivity_out = folded(
            ctx,
            "diffusivity_in",
            material.damping_diffusivity(density).unwrap_or(0.0) as f32,
        );
        let density_out = folded(ctx, "density_in", density as f32);
        // The walls take the first item's friction; unwired, this is it.
        let handed_friction = ctx.scalar_or_param("wall_friction_in", -1.0);
        let wall_friction = if handed_friction >= 0.0 {
            handed_friction
        } else {
            material.wall_friction() as f32
        };
        for (name, value) in [
            ("wave", wave_out),
            ("snow_wave", snow_wave_out),
            ("diffusivity", diffusivity_out),
            ("density", density_out),
            ("wall_friction", wall_friction),
        ] {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }

        let row = material.row(density as f32);
        let handed = ctx.inputs.array("materials_in");
        let table = ctx.outputs.array("materials");
        let gpu = ctx.gpu_encoder();
        if self.row != Some(row) || self.upload.is_none() {
            let upload = gpu.device.create_buffer(ROW_BYTES, BufferUse::Upload);
            // SAFETY: a new shared buffer no GPU work has seen yet.
            unsafe { upload.write(0, bytemuck::cast_slice(&row)) };
            self.upload = Some(upload);
            self.row = Some(row);
        }
        let Some(table) = table.filter(|table| table.size >= TABLE_BYTES) else {
            return;
        };
        match handed.filter(|handed| handed.size >= TABLE_BYTES) {
            Some(handed) => gpu.native_enc.copy_buffer_to_buffer(handed, table, TABLE_BYTES),
            None => gpu.native_enc.clear_buffer(table),
        }
        let upload = self.upload.as_ref().expect("row uploaded above");
        gpu.native_enc
            .copy_buffer_range(upload, 0, table, slot as u64 * ROW_BYTES, ROW_BYTES);
    }
}
