//! `node.generate_range` — emit an `Array<f32>` of `N` samples
//! linearly spaced over `[start, end]`. The Pattern-CHOP atom: the
//! source of an evenly-sampled parameter sweep that downstream
//! `array_math` / `array_trig` / curve-pack primitives consume.
//!
//! Canonical use: build a `t` array spanning `[0, 2π]` to drive a
//! parametric curve (Lissajous, Rose, hypocycloid, audio waveform).
//! Pair with `array_math(ScaleOffset)` + `array_math(Sin)` + sibling
//! axis chain + `pack_curve_xy` for a fully-decomposed parametric
//! curve graph.
//!
//! Sample i (for i in `[0, count)`):
//!   `out[i] = start + i * (end - start) / max(count - 1, 1)`
//!
//! So `out[0] = start`, `out[count - 1] = end`, evenly spaced. Matches
//! numpy / TouchDesigner `linspace` semantics. For `count == 1` the
//! divisor floors to `1` and the single sample emitted is `start`.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RangeUniforms {
    count: u32,
    // 1 = end-inclusive (denom = max(count - 1, 1)); 0 = end-exclusive
    // (denom = count). Carries the param-side toggle into the shader.
    end_inclusive: u32,
    start: f32,
    end: f32,
}

crate::primitive! {
    name: GenerateRange,
    type_id: "node.generate_range",
    purpose: "Emit an Array<f32> of `count` samples linearly spaced over `[start, end]`. The TouchDesigner Pattern-CHOP analogue. Output is the t-parameter array that drives parametric curve graphs — pair with array_math (ScaleOffset / Sin / Cos) + pack_curve_xy for Lissajous-style curves, or with cycle_table_row + mux for stepped sequences. `start` and `end` are port-shadows-param so an LFO or driver wire can sweep the range dynamically. `active_count` (input-only) overrides the sample count for variable-N curves (regular polygons, variable-resolution sweeps) — when unwired the count is `max_capacity` for backward-compatible fixed-resolution sampling. Output capacity is `max_capacity`.",
    inputs: {
        start: ScalarF32 optional,
        end: ScalarF32 optional,
        active_count: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: "start",
            label: "Start",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "end",
            label: "End",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "max_capacity",
            label: "Sample Count",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "end_inclusive",
            label: "End Inclusive",
            ty: ParamType::Bool,
            // Default true preserves the conventional [start, end] sampling
            // for closed parametric curves (Lissajous and friends rely on
            // `out[0]=start`, `out[count-1]=end` so vertex 0 and vertex
            // count-1 land on the same point of the cycle). Set false for
            // end-exclusive sampling that's the right shape for regular
            // N-gons (linspace(0, 2π, N) end-exclusive → angles
            // [0, 2π/N, 4π/N, …, (N-1)·2π/N], no duplicated wrap vertex).
            default: ParamValue::Bool(true),
            range: None,
            enum_values: &[],
        },
    ],
    composition_notes: "End-inclusive (default): `out[0]=start`, `out[count-1]=end` — conventional for closed parametric curves where the start and end of the cycle are meant to land on the same point (Lissajous). End-exclusive: `out[0]=start`, `out[count-1]=end - (end-start)/count` — the right shape for regular N-gons sampled around a circle, where vertex 0 and vertex count-1 must be distinct points. `active_count` (input-only port-shadow) sets the runtime sample count when wired; when unwired the count is `max_capacity` so old graphs (Lissajous) work bit-identically. `max_capacity` is the pre-allocated buffer size — the chain build reads it via the default `array_output_capacity` impl, so `active_count` cannot exceed it. Slots beyond `active_count` retain their previous-frame values, which is harmless when downstream topology (consecutive_edges / explicit EdgePair sentinels) refuses to reference them.",
    examples: [],
    picker: { label: "Generate Range", category: Atom },
}

impl Primitive for GenerateRange {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let start = ctx.scalar_or_param("start", 0.0);
        let end = ctx.scalar_or_param("end", 1.0);
        let max_count = match ctx.params.get("max_capacity") {
            Some(ParamValue::Float(f)) => f.round().max(2.0) as u32,
            _ => 256,
        };
        // Port-shadow: when `active_count` is wired, it overrides
        // `max_capacity` as the sample count. Rounded + clamped to
        // [2, max_capacity]; default (unwired) is max_capacity so old
        // graphs (Lissajous) dispatch identically.
        let count = match ctx.inputs.scalar("active_count").and_then(|v| v.as_scalar()) {
            Some(n) => (n.round().max(2.0) as u32).min(max_count),
            None => max_count,
        };
        let end_inclusive = matches!(
            ctx.params.get("end_inclusive"),
            Some(ParamValue::Bool(true)) | None
        );

        let Some(out_buf) = ctx.outputs.array("out") else {
            log::warn!(
                "node.generate_range: no GpuBuffer bound to output port `out` — \
                 the chain build did not pre-allocate this Array<f32>, so the \
                 range generator is a no-op this frame. Confirm `max_capacity` \
                 is set on this node.",
            );
            return;
        };
        let f32_size = std::mem::size_of::<f32>() as u64;
        let capacity = (out_buf.size / f32_size) as u32;
        let active_count = count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_range.wgsl"),
                "cs_main",
                "node.generate_range",
            )
        });

        let uniforms = RangeUniforms {
            count: active_count,
            end_inclusive: end_inclusive as u32,
            start,
            end,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [active_count.div_ceil(64), 1, 1],
            "node.generate_range",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_three_optional_scalar_inputs_and_one_f32_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(GenerateRange::TYPE_ID, "node.generate_range");
        assert_eq!(GenerateRange::INPUTS.len(), 3);
        for port in GenerateRange::INPUTS {
            assert!(!port.required, "{} must be optional (port-shadow)", port.name);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
        let input_names: Vec<&str> = GenerateRange::INPUTS.iter().map(|p| p.name).collect();
        assert_eq!(input_names, vec!["start", "end", "active_count"]);

        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(GenerateRange::OUTPUTS.len(), 1);
        assert_eq!(GenerateRange::OUTPUTS[0].name, "out");
        assert_eq!(GenerateRange::OUTPUTS[0].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn params_cover_start_end_capacity_and_end_inclusive() {
        let names: Vec<&str> = GenerateRange::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["start", "end", "max_capacity", "end_inclusive"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GenerateRange::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_range");
    }
}
