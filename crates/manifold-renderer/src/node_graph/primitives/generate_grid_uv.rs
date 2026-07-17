//! `node.grid_points` — emit two `Array<f32>` outputs sampling
//! a 2D parameter domain `[0, u_max) × [0, v_max)` at `grid_size`
//! steps along each axis, flattened to `grid_size²` entries in
//! row-major order: `idx = iu * grid_size + iv`.
//!
//! The Pattern-CHOP-of-a-grid: pair with `array_math` (Cos / Sin /
//! ScaleOffset) + `pack_vec4` + `edges_from_grid_uv` to author any
//! (u, v)-parametric surface in the graph (Duocylinder, torus, Klein,
//! geodesic sphere, terrain mesh).
//!
//! Sampling is end-exclusive: `u[idx] = iu * (u_max / grid_size)`,
//! `v[idx] = iv * (v_max / grid_size)`. With the default `u_max =
//! v_max = TAU` this gives `grid_size` distinct points around each
//! period, the natural shape for periodic surfaces where the wrap is
//! supplied by `edges_from_grid_uv` (next-neighbor with modular wrap).
//!
//! `grid_size` is an Int param read at plan time to size the two
//! output buffers; changing it triggers a chain rebuild. `u_max` and
//! `v_max` are Float params with port-shadows so an LFO can sweep
//! the domain at performance time.
//!
//! CPU-only: at most a few thousand f32s per frame (64² × 2 channels =
//! 8K floats at max grid size), faster on CPU than GPU dispatch
//! overhead and lands the data in shared MTLBuffer for downstream
//! CPU consumers (`pack_vec4`, `edges_from_grid_uv`) to read same-frame.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const GRID_UV_DEFAULT_SIZE: u32 = 24;
pub const GRID_UV_MAX_SIZE: u32 = 64;
pub const TAU: f32 = std::f32::consts::TAU;

crate::primitive! {
    name: GenerateGridUv,
    type_id: "node.grid_points",
    purpose: "Emit two Array<f32> outputs (u_values, v_values) sampling a 2D parameter domain [0, u_max) × [0, v_max) at grid_size steps along each axis, flattened to grid_size² entries in row-major order (idx = iu * grid_size + iv). The parametric-surface authoring atom: pair with array_math (Cos / Sin / ScaleOffset) + pack_vec4 + edges_from_grid_uv to author any (u, v)-parametric surface in the graph — Duocylinder, torus, Klein bottle, geodesic sphere, terrain mesh — without a per-surface Rust atom. End-exclusive sampling matches periodic-surface conventions where the wrap edge is supplied by edges_from_grid_uv. CPU-only — runs on the content thread so downstream CPU readers see same-frame writes.",
    inputs: {
        u_max: ScalarF32 optional,
        v_max: ScalarF32 optional,
    },
    outputs: {
        u_values: Array(f32),
        v_values: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("grid_size"),
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Float(GRID_UV_DEFAULT_SIZE as f32),
            range: Some((2.0, GRID_UV_MAX_SIZE as f32)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("u_max"),
            label: "U Max",
            ty: ParamType::Float,
            default: ParamValue::Float(TAU),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("v_max"),
            label: "V Max",
            ty: ParamType::Float,
            default: ParamValue::Float(TAU),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "End-exclusive sampling: u[iu*n+iv] = iu * (u_max/n), v[iu*n+iv] = iv * (v_max/n). u_max and v_max default to TAU so a default-configured grid sweeps one full period along each axis — the right shape for closed parametric surfaces (torus / Duocylinder / sphere). For terrain-style open meshes set both to 1.0 and treat the sweep as normalised UV. grid_size is read at plan time to size the buffers (grid_size²); changing it at runtime triggers a chain rebuild — drive `active_count` through an outer-card slider only at authoring time. u_max / v_max are port-shadowed so an LFO can sweep the domain at performance time without recompilation.",
    examples: [],
    picker: { label: "Grid Points (UV)", category: Atom },
    summary: "Outputs a grid of U and V values sampling a parametric surface, the input for building curved meshes and wireframes.",
    category: Geometry3D,
    role: Source,
    aliases: ["grid points", "generate grid uv", "uv grid", "parametric"],
    boundary_reason: NonGpu,
}

/// Read `grid_size` from the params bag, clamped to the valid range.
/// Shared by `array_output_capacity` (plan time) and `run` (frame time)
/// so buffer sizing always matches the sample loop.
fn read_grid_size(params: &crate::node_graph::effect_node::ParamValues) -> u32 {
    match params.get("grid_size") {
        Some(ParamValue::Float(n)) => n.round().max(2.0) as u32,
        _ => GRID_UV_DEFAULT_SIZE,
    }
    .min(GRID_UV_MAX_SIZE)
}

impl Primitive for GenerateGridUv {
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let n = read_grid_size(params);
        match port_name {
            "u_values" | "v_values" => Some(n * n),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let n = read_grid_size(ctx.params);
        let u_max = ctx.scalar_or_param("u_max", TAU);
        let v_max = ctx.scalar_or_param("v_max", TAU);

        let Some(u_buf) = ctx.outputs.array("u_values") else {
            log::warn!(
                "node.grid_points: no GpuBuffer bound to output port `u_values` — \
                 the chain build did not pre-allocate this Array<f32>.",
            );
            return;
        };
        let Some(v_buf) = ctx.outputs.array("v_values") else {
            log::warn!(
                "node.grid_points: no GpuBuffer bound to output port `v_values` — \
                 the chain build did not pre-allocate this Array<f32>.",
            );
            return;
        };

        let f32_size = std::mem::size_of::<f32>() as u64;
        let u_capacity = (u_buf.size / f32_size) as u32;
        let v_capacity = (v_buf.size / f32_size) as u32;
        let count = (n * n).min(u_capacity).min(v_capacity);
        if count == 0 {
            return;
        }

        const SCRATCH_LEN: usize = (GRID_UV_MAX_SIZE * GRID_UV_MAX_SIZE) as usize;
        let mut u_scratch = [0.0_f32; SCRATCH_LEN];
        let mut v_scratch = [0.0_f32; SCRATCH_LEN];
        let u_step = u_max / (n as f32);
        let v_step = v_max / (n as f32);
        let write_count = (count as usize).min(SCRATCH_LEN);
        for iu in 0..n {
            for iv in 0..n {
                let idx = (iu * n + iv) as usize;
                if idx >= write_count {
                    break;
                }
                u_scratch[idx] = (iu as f32) * u_step;
                v_scratch[idx] = (iv as f32) * v_step;
            }
        }

        // Safety: shared-memory MTLBuffers pre-bound by the chain build;
        // write count clamped to both buffer capacities; sequential
        // executor on the content thread means no concurrent writer.
        unsafe {
            u_buf.write(0, bytemuck::cast_slice(&u_scratch[..write_count]));
            v_buf.write(0, bytemuck::cast_slice(&v_scratch[..write_count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_optional_umax_vmax_inputs_and_two_f32_outputs() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(GenerateGridUv::TYPE_ID, "node.grid_points");
        assert_eq!(GenerateGridUv::INPUTS.len(), 2);
        for port in GenerateGridUv::INPUTS {
            assert!(!port.required, "{} must be optional", port.name);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(GenerateGridUv::OUTPUTS.len(), 2);
        assert_eq!(GenerateGridUv::OUTPUTS[0].name, "u_values");
        assert_eq!(GenerateGridUv::OUTPUTS[1].name, "v_values");
        assert_eq!(GenerateGridUv::OUTPUTS[0].ty, PortType::Array(f32_layout));
        assert_eq!(GenerateGridUv::OUTPUTS[1].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn output_capacity_scales_with_grid_size() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = GenerateGridUv::new();

        let default = ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "u_values", &default, &[]),
            Some(GRID_UV_DEFAULT_SIZE * GRID_UV_DEFAULT_SIZE),
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "v_values", &default, &[]),
            Some(GRID_UV_DEFAULT_SIZE * GRID_UV_DEFAULT_SIZE),
        );

        let mut custom = ParamValues::default();
        custom.insert(std::borrow::Cow::Borrowed("grid_size"), ParamValue::Float(16.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "u_values", &custom, &[]),
            Some(16 * 16),
        );

        let mut huge = ParamValues::default();
        huge.insert(std::borrow::Cow::Borrowed("grid_size"), ParamValue::Float(128.0));
        assert_eq!(
            Primitive::array_output_capacity(&prim, "u_values", &huge, &[]),
            Some(GRID_UV_MAX_SIZE * GRID_UV_MAX_SIZE),
        );

        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &default, &[]),
            None,
        );
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = GenerateGridUv::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.grid_points");
    }
}
