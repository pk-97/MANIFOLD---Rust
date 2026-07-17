//! `node.array_math` — element-wise arithmetic over `Array<f32>`(s).
//!
//! Array-domain counterpart to `node.math` (scalar). One bundled
//! primitive with an op enum: binary ops read `a` and `b`; unary
//! ops read `a` only and ignore `b`. Op-specific scalars (`scale`
//! / `offset` / `exp` / `bias`) are port-shadow-param so they can
//! be driven by control wires.
//!
//! Same composition style as `node.math`: one node where you'd
//! otherwise reach for many tiny atomic-op primitives. The cost
//! is conditional param semantics (each op only uses a subset of
//! the param surface); the benefit is one entry in the registry
//! that covers the common Array<f32> shaping vocabulary.
//!
//! CPU-only: small array sizes (curve sources ship at most a few
//! hundred f32s per frame), CPU dispatch is faster than the GPU
//! pipeline overhead and — critically — keeps the curve-math chain
//! on the content thread so downstream CPU primitives (replicators,
//! polyline stackers) can read same-frame writes via `mapped_ptr`
//! without a GPU→CPU fence.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Op enum labels. **Indices are public API** — saved in JSON presets —
/// never reorder; append new ops at the end.
pub const ARRAY_MATH_OPS: &[&str] = &[
    "Add",          // 0  binary: out = a + b
    "Subtract",     // 1  binary: out = a - b
    "Multiply",     // 2  binary: out = a * b
    "Divide",       // 3  binary: out = a / b  (|b| < eps → 0)
    "Min",          // 4  binary: out = min(a, b)
    "Max",          // 5  binary: out = max(a, b)
    "ScaleOffset",  // 6  unary:  out = a * scale + offset
    "ShapePowClip", // 7  unary:  out = pow(max(a + bias, 0), exp) * scale
    "MirrorRamp",   // 8  unary:  out = smoothstep(0, 1, 1 - |2a - 1|)
    "Clamp01",      // 9  unary:  out = clamp(a, 0, 1)
    "Abs",          // 10 unary:  out = |a|
    "Sin",          // 11 unary:  out = sin(a)
    "Cos",          // 12 unary:  out = cos(a)
    "Mix",          // 13 binary: out = a + (b - a) * scale  (lerp; `scale` is t)
];

/// First op index that does NOT read `b` in the original
/// 0..=10 contiguous block. Ops appended past index 10 may be binary
/// or unary on a per-op basis — see [`op_is_binary`].
const FIRST_UNARY_OP: u32 = 6;

/// Whether op `code` reads the `b` input (and therefore needs the
/// processing range truncated to `min(a, b, out)` capacity). Replaces
/// the single-threshold partition once non-contiguous binary ops (Mix)
/// were appended after the unary block.
fn op_is_binary(code: u32) -> bool {
    code < FIRST_UNARY_OP || code == 13
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn eval_op(op: u32, a: f32, b: f32, scale: f32, offset: f32, exp: f32, bias: f32) -> f32 {
    match op {
        0 => a + b,
        1 => a - b,
        2 => a * b,
        3 => {
            if b.abs() < 1e-6 {
                0.0
            } else {
                a / b
            }
        }
        4 => a.min(b),
        5 => a.max(b),
        6 => a * scale + offset,
        7 => (a + bias).max(0.0).powf(exp) * scale,
        8 => smoothstep(0.0, 1.0, 1.0 - (2.0 * a - 1.0).abs()),
        9 => a.clamp(0.0, 1.0),
        10 => a.abs(),
        11 => a.sin(),
        12 => a.cos(),
        13 => a + (b - a) * scale,
        // Unknown op codes resolve to passthrough rather than panic;
        // a stale preset referencing a renamed op shouldn't crash the
        // content thread.
        _ => a,
    }
}

crate::primitive! {
    name: ArrayMath,
    type_id: "node.array_math",
    purpose: "Element-wise math over Array<f32>. One bundled primitive (op enum) covering the common shaping vocabulary: binary (Add/Subtract/Multiply/Divide/Min/Max/Mix read `a` + `b`); unary (ScaleOffset, ShapePowClip, MirrorRamp, Clamp01, Abs, Sin, Cos — read `a` only, ignore `b`). Op-specific scalars (scale / offset / exp / bias) are port-shadow-param so they can be modulated by control wires. Divide-by-near-zero clamps to 0 to keep NaN/Inf out of downstream consumers. CPU-only — runs on the content thread so downstream CPU readers see same-frame writes.",
    inputs: {
        a: Array(f32) required,
        b: Array(f32) optional,
        scale: ScalarF32 optional,
        offset: ScalarF32 optional,
        exp: ScalarF32 optional,
        bias: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("op"),
            label: "Operation",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // Multiply — most useful default
            range: Some((0.0, (ARRAY_MATH_OPS.len() - 1) as f32)),
            enum_values: ARRAY_MATH_OPS,
        },
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset"),
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("exp"),
            label: "Exponent",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("bias"),
            label: "Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Each op uses a subset of (scale, offset, exp, bias) — only the relevant scalars are read per op. ScaleOffset uses scale + offset; ShapePowClip uses bias + exp + scale (captures the DigitalPlants stem displacement pow(max(x, 0), 2) * 0.3 shape with bias=0, exp=2, scale=0.3); Mix uses scale as the lerp factor t (out = a + (b - a) * scale; the port-shadowed `scale` input drives the morph from a wire); Sin / Cos / MirrorRamp / Clamp01 / Abs read none. Binary ops (Add/Sub/Mul/Div/Min/Max/Mix) require both `a` and `b` wired; unary ops only require `a`. CPU dispatch reads inputs via `mapped_ptr` and writes the output via `write()` on the shared MTLBuffer.",
    examples: [],
    picker: { label: "Array Math", category: Atom },
    summary: "Runs the same math over every number in a list, like add, multiply, sine, or scale. The list-wide version of the Math node.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["list math", "array math", "Math CHOP"],
    boundary_reason: NonGpu,
}

impl Primitive for ArrayMath {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "a")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let op = match ctx.params.get("op") {
            Some(ParamValue::Enum(n)) => (*n).min(ARRAY_MATH_OPS.len() as u32 - 1),
            Some(ParamValue::Float(f)) => {
                (f.round().max(0.0) as u32).min(ARRAY_MATH_OPS.len() as u32 - 1)
            }
            _ => 2,
        };
        let scale = ctx.scalar_or_param("scale", 1.0);
        let offset = ctx.scalar_or_param("offset", 0.0);
        let exp = ctx.scalar_or_param("exp", 1.0);
        let bias = ctx.scalar_or_param("bias", 0.0);

        let Some(a_buf) = ctx.inputs.array("a") else {
            return;
        };
        let b_buf_opt = ctx.inputs.array("b");
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let f32_size = std::mem::size_of::<f32>() as u64;
        let a_capacity = (a_buf.size / f32_size) as u32;
        let b_capacity = b_buf_opt
            .map(|buf| (buf.size / f32_size) as u32)
            .unwrap_or(a_capacity);
        let out_capacity = (out_buf.size / f32_size) as u32;
        let count = if op_is_binary(op) {
            // Binary: process across the smaller of a, b, out so we
            // never read past either input.
            a_capacity.min(b_capacity).min(out_capacity)
        } else {
            // Unary: only `a` matters; `b` is unread.
            a_capacity.min(out_capacity)
        };
        if count == 0 {
            return;
        }

        // ── Read inputs via mapped_ptr ──
        // Shared-memory MTLBuffers; sequential executor on the content
        // thread means upstream writes (also CPU) are visible by the
        // time we run.
        let a_ptr = a_buf
            .mapped_ptr()
            .expect("array_math: `a` input must be shared-memory");
        let a_slice: &[f32] = unsafe {
            std::slice::from_raw_parts(a_ptr as *const f32, a_capacity as usize)
        };
        // For unary ops the `b` slot is unread — pass an empty slice
        // and `eval_op` ignores the value. For binary ops we require
        // a real `b` source: when not wired, fall back to `a` so the
        // primitive behaves like the GPU version did with the
        // `b_buf.unwrap_or(a_buf)` aliasing (Mix without a wired `b`
        // becomes identity).
        let b_slice: &[f32] = match b_buf_opt {
            Some(buf) => {
                let ptr = buf
                    .mapped_ptr()
                    .expect("array_math: `b` input must be shared-memory");
                unsafe { std::slice::from_raw_parts(ptr as *const f32, b_capacity as usize) }
            }
            None => a_slice,
        };

        // ── Compute into stack scratch then bulk-write ──
        // Output bursts at most 4096 f32s in practice (matches the
        // generate_range capacity cap); stack-allocate to avoid a
        // per-frame Vec.
        const SCRATCH_LEN: usize = 4096;
        let mut scratch = [0.0_f32; SCRATCH_LEN];
        let write_count = (count as usize).min(SCRATCH_LEN);
        for (i, slot) in scratch.iter_mut().take(write_count).enumerate() {
            let a = a_slice[i];
            let b = if op_is_binary(op) { b_slice[i] } else { 0.0 };
            *slot = eval_op(op, a, b, scale, offset, exp, bias);
        }

        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity above; sequential
        // executor on the content thread means no concurrent writer.
        unsafe {
            out_buf.write(0, bytemuck::cast_slice(&scratch[..write_count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn array_math_declares_a_required_b_optional_and_one_f32_out() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(ArrayMath::TYPE_ID, "node.array_math");

        let a_in = ArrayMath::INPUTS.iter().find(|p| p.name == "a").unwrap();
        assert!(a_in.required);
        assert_eq!(a_in.ty, PortType::Array(f32_layout));

        let b_in = ArrayMath::INPUTS.iter().find(|p| p.name == "b").unwrap();
        assert!(!b_in.required);
        assert_eq!(b_in.ty, PortType::Array(f32_layout));

        for name in ["scale", "offset", "exp", "bias"] {
            let port = ArrayMath::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(ArrayMath::OUTPUTS.len(), 1);
        assert_eq!(ArrayMath::OUTPUTS[0].name, "out");
        assert_eq!(ArrayMath::OUTPUTS[0].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn array_math_ops_table_covers_binary_and_unary_partition() {
        // Op indices are public API (saved in JSON presets / param
        // values) — this guards against accidental reordering.
        assert_eq!(ARRAY_MATH_OPS[0], "Add");
        assert_eq!(ARRAY_MATH_OPS[2], "Multiply");
        assert_eq!(ARRAY_MATH_OPS[5], "Max");
        assert_eq!(ARRAY_MATH_OPS[6], "ScaleOffset");
        assert_eq!(ARRAY_MATH_OPS[7], "ShapePowClip");
        assert_eq!(ARRAY_MATH_OPS[8], "MirrorRamp");
        assert_eq!(ARRAY_MATH_OPS[9], "Clamp01");
        assert_eq!(ARRAY_MATH_OPS[10], "Abs");
        assert_eq!(ARRAY_MATH_OPS[11], "Sin");
        assert_eq!(ARRAY_MATH_OPS[12], "Cos");
        assert_eq!(ARRAY_MATH_OPS[13], "Mix");
        assert_eq!(
            FIRST_UNARY_OP, 6,
            "the original 0..=10 contiguous block keeps the binary/unary threshold",
        );
        assert_eq!(ARRAY_MATH_OPS.len(), 14);
    }

    #[test]
    fn op_is_binary_classifies_each_op_correctly() {
        for code in 0..6 {
            assert!(op_is_binary(code), "{} (idx {code}) is binary", ARRAY_MATH_OPS[code as usize]);
        }
        for code in 6..=12 {
            assert!(!op_is_binary(code), "{} (idx {code}) is unary", ARRAY_MATH_OPS[code as usize]);
        }
        assert!(op_is_binary(13), "Mix (idx 13) is binary");
    }

    #[test]
    fn array_math_output_capacity_follows_a_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = ArrayMath::new();
        let params = ParamValues::default();
        let inputs = [("a", 160_000_u32), ("b", 100_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(160_000),
            "output sized to `a`; binary ops truncate processing at run time",
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ArrayMath::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.array_math");
    }
}
