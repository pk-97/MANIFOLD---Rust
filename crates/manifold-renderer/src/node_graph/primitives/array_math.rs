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

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Op enum labels — order MUST match the `switch u.op` block in
/// `shaders/array_math.wgsl`. Adding a variant means adding the
/// shader case in the same commit. **Indices are public API** — saved
/// in JSON presets — never reorder; append new ops at the end.
pub const ARRAY_MATH_OPS: &[&str] = &[
    "Add",          // 0  binary: out = a + b
    "Subtract",     // 1  binary: out = a - b
    "Multiply",     // 2  binary: out = a * b
    "Divide",       // 3  binary: out = a / b  (b == 0 → 0)
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
/// dispatch truncated to `min(a, b, out)` capacity). Replaces the
/// single-threshold partition once non-contiguous binary ops (Mix)
/// were appended after the unary block. WGSL mirror lives in
/// `shaders/array_math.wgsl`'s analogous `op_is_binary` helper —
/// keep the two in sync.
fn op_is_binary(code: u32) -> bool {
    code < FIRST_UNARY_OP || code == 13
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    count: u32,
    op: u32,
    scale: f32,
    offset: f32,
    exp: f32,
    bias: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: ArrayMath,
    type_id: "node.array_math",
    purpose: "Element-wise math over Array<f32>. One bundled primitive (op enum) covering the common shaping vocabulary: binary (Add/Subtract/Multiply/Divide/Min/Max/Mix read `a` + `b`); unary (ScaleOffset, ShapePowClip, MirrorRamp, Clamp01, Abs, Sin, Cos — read `a` only, ignore `b`). Op-specific scalars (scale / offset / exp / bias) are port-shadow-param so they can be modulated by control wires. Divide-by-near-zero clamps to 0 to keep NaN/Inf out of downstream shaders.",
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
            name: "op",
            label: "Operation",
            ty: ParamType::Enum,
            default: ParamValue::Enum(2), // Multiply — most useful default
            range: Some((0.0, (ARRAY_MATH_OPS.len() - 1) as f32)),
            enum_values: ARRAY_MATH_OPS,
        },
        ParamDef {
            name: "scale",
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset",
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "exp",
            label: "Exponent",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "bias",
            label: "Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Each op uses a subset of (scale, offset, exp, bias) — only the relevant scalars are read per op. ScaleOffset uses scale + offset; ShapePowClip uses bias + exp + scale (captures the DigitalPlants stem displacement pow(max(x, 0), 2) * 0.3 shape with bias=0, exp=2, scale=0.3); Mix uses scale as the lerp factor t (out = a + (b - a) * scale; the port-shadowed `scale` input drives the morph from a wire); Sin / Cos / MirrorRamp / Clamp01 / Abs read none. Binary ops (Add/Sub/Mul/Div/Min/Max/Mix) require both `a` and `b` wired; unary ops only require `a` (the `b` slot is ignored — internally the primitive falls back to binding `a` to the `b` slot to satisfy Metal's all-bindings-present rule).",
    examples: [],
    picker: { label: "Array Math", category: Atom },
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
        // Output sized to `a` — for binary ops, run() truncates the
        // dispatch to min(a, b) so trailing elements stay at whatever
        // the buffer held (typically zero from chain-build allocation).
        // Sizing to `a` is the consistent shape across binary + unary.
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
        let b_buf = ctx.inputs.array("b").unwrap_or(a_buf);
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let f32_size = std::mem::size_of::<f32>() as u64;
        let a_capacity = (a_buf.size / f32_size) as u32;
        let b_capacity = (b_buf.size / f32_size) as u32;
        let out_capacity = (out_buf.size / f32_size) as u32;
        let count = if op_is_binary(op) {
            // Binary: dispatch across the smaller of a, b, out so the
            // shader never reads past either input.
            a_capacity.min(b_capacity).min(out_capacity)
        } else {
            // Unary: only `a` matters. `b` is aliased to `a` for the
            // Metal-binding-present requirement; its content is unread.
            a_capacity.min(out_capacity)
        };
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/array_math.wgsl"),
                "cs_main",
                "node.array_math",
            )
        });

        let uniforms = Uniforms {
            count,
            op,
            scale,
            offset,
            exp,
            bias,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: a_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: b_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.array_math",
        );
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

    /// Per-op binary classification. The original `op < FIRST_UNARY_OP`
    /// threshold only worked while binary ops formed a contiguous prefix
    /// — Mix (appended at index 13 to preserve JSON op-index API) is
    /// binary but lives past the threshold, so the classifier must be
    /// per-op now. Guards against accidentally letting Mix dispatch
    /// without truncating to `min(a, b, out)`.
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
            "output sized to `a`; binary ops truncate dispatch at run time",
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ArrayMath::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.array_math");
    }
}
